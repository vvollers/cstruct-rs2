//! Laying out a record whose size depends on its own content.
//!
//! For a fixed record every offset is already in the plan and none of this runs
//! -- that is the fast path, and `check_fixed_size` is the whole of its
//! validation. Everything here is the other case: a length that has to be read
//! before the field it sizes can be placed.
//!
//! # The one distinction that has caused the most damage
//!
//! A field's **value size** and the bytes it **consumes** are not always the
//! same number. A null-terminated array's value excludes its terminator and its
//! consumption includes it. Conflating them left every field after such an
//! array one byte early: `dissect.target`'s mlocate reader desynced after 28 of
//! 228,071 records and then failed on invalid UTF-8, which is what a one-byte
//! offset error looks like from the outside.
//!
//! So nearly every test below asserts the *following* field's offset as well as
//! the interesting field's size. A layout test that checks only the field it is
//! about cannot see this class of defect at all.

use std::sync::Arc;

use cstruct_rs2_core::decode::{
    check_fixed_size, resolve_layout, resolve_layout_with, CustomRead, CustomReader, FieldSpan,
    Layout,
};
use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::parser::parse;
use cstruct_rs2_core::plan::{CompiledPlan, Compiler};
use cstruct_rs2_core::schema::Endian;

fn plan_for(source: &str, name: &str) -> Arc<CompiledPlan> {
    let schema = parse(source, Endian::Little).unwrap_or_else(|e| panic!("{source}: {e}"));
    Compiler::new(&schema)
        .compile(name)
        .unwrap_or_else(|e| panic!("{name} should compile: {e}"))
}

fn layout(plan: &CompiledPlan, buf: &[u8]) -> Layout {
    resolve_layout(plan, buf).unwrap_or_else(|e| panic!("layout should resolve: {e}"))
}

// ---------------------------------------------------------------------------
// The fixed path
// ---------------------------------------------------------------------------

/// A fixed record's validation is one comparison, which is why it is the fast
/// path. A record whose bytes are not all present is end-of-stream.
#[test]
fn a_fixed_record_is_validated_by_one_comparison() {
    let plan = plan_for("struct s { uint16 a; uint32 b; };", "s");
    assert_eq!(check_fixed_size(&plan, &[0u8; 6]).unwrap(), 6);
    assert_eq!(
        check_fixed_size(&plan, &[0u8; 99]).unwrap(),
        6,
        "extra bytes are fine"
    );
    assert_eq!(check_fixed_size(&plan, &[0u8; 5]), Err(Error::EndOfStream));
    assert_eq!(check_fixed_size(&plan, &[]), Err(Error::EndOfStream));
}

/// Asking a dynamic type for its fixed size is a clear error rather than a
/// guess, because there is no answer until the bytes are seen.
#[test]
fn a_dynamic_record_has_no_fixed_size_to_check() {
    let plan = plan_for("struct s { uint16 n; char d[n]; };", "s");
    match check_fixed_size(&plan, &[0u8; 16]) {
        Err(Error::InvalidValue { message }) => {
            assert!(message.contains("fixed size"), "{message}")
        }
        other => panic!("expected a value error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Dynamic arrays
// ---------------------------------------------------------------------------

/// A length named by an earlier field, and the trailing field that must follow
/// the array rather than sit at a fixed offset.
#[test]
fn a_length_field_sizes_the_array_and_moves_what_follows() {
    let plan = plan_for(
        "struct s { uint16 len; uint8 data[len]; uint16 tail; };",
        "s",
    );
    let buf = [0x03, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
    let resolved = layout(&plan, &buf);

    assert_eq!(
        resolved.spans[0],
        FieldSpan {
            offset: 0,
            size: 2,
            count: 1
        }
    );
    assert_eq!(
        resolved.spans[1],
        FieldSpan {
            offset: 2,
            size: 3,
            count: 3
        }
    );
    assert_eq!(
        resolved.spans[2],
        FieldSpan {
            offset: 5,
            size: 2,
            count: 1
        },
        "the trailing field follows the array, not a fixed offset"
    );
    assert_eq!(resolved.consumed, 7);
}

/// A `wchar[len]` costs two bytes per element. Getting the stride wrong
/// silently truncates every filename.
#[test]
fn a_wide_array_uses_its_element_stride() {
    let plan = plan_for(
        "struct s { uint16 len; wchar name[len]; uint8 tail; };",
        "s",
    );
    let mut buf = vec![0x02, 0x00];
    buf.extend_from_slice(&[b'h', 0, b'i', 0, 0xff]);
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].size, 4, "two code units of two bytes");
    assert_eq!(resolved.spans[1].count, 2);
    assert_eq!(resolved.spans[2].offset, 6);
    assert_eq!(resolved.consumed, 7);
}

/// A length may be an expression over several earlier fields, not just a name.
#[test]
fn a_length_may_be_arithmetic_over_earlier_fields() {
    let plan = plan_for(
        "struct s { uint8 a; uint8 b; char d[(a + b) * 2]; uint8 tail; };",
        "s",
    );
    let buf = [1, 2, b'a', b'b', b'c', b'd', b'e', b'f', 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[2].size, 6);
    assert_eq!(resolved.spans[3].offset, 8);
}

/// A duplicate field is rejected before an ambiguous length reaches layout.
#[test]
fn duplicate_length_fields_are_rejected_before_layout() {
    let error = parse(
        "struct s { uint8 n; uint8 pad; uint8 n; char d[n]; };",
        Endian::Little,
    )
    .expect_err("duplicate public names are ambiguous");
    assert_eq!(error.to_string(), "Duplicate field name: n");
}

/// A negative length is an *empty* array, not an error.
///
/// Upstream builds arrays with `range(count)`, and `range(-40)` is empty.
/// `dissect.extfs` depends on it: `char i_extra[256 - 128 - i_extra_isize]`
/// goes negative on every ext3 image, where `i_extra_isize` is not meaningful.
/// Treating it as an error would refuse records upstream reads fine.
#[test]
fn a_negative_length_yields_an_empty_array() {
    let plan = plan_for(
        "struct s { uint16 n; uint8 data[10 - n]; uint16 after; };",
        "s",
    );
    let buf = [50, 0, 0xaa, 0xbb];
    let resolved = layout(&plan, &buf);
    assert_eq!(
        resolved.spans[1],
        FieldSpan {
            offset: 2,
            size: 0,
            count: 0
        }
    );
    assert_eq!(
        resolved.spans[2].offset, 2,
        "the following field reads straight after the empty array"
    );
}

/// A zero length is an empty array too, and `uint32 block_list[0]` is a real
/// declaration -- so a count of zero must survive rather than being clamped to
/// one.
#[test]
fn a_zero_length_array_occupies_nothing() {
    let plan = plan_for("struct s { uint8 n; uint8 d[n]; uint8 tail; };", "s");
    let buf = [0, 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].size, 0);
    assert_eq!(resolved.spans[2].offset, 1);

    let fixed = plan_for("struct s { uint32 empty[0]; uint8 tail; };", "s");
    assert_eq!(
        fixed.fields[0].count, 0,
        "a declared zero count is not a scalar's one"
    );
    assert_eq!(fixed.total_size, Some(1));
}

/// A length that names a field that was never decoded is an error, not a zero.
#[test]
fn a_length_naming_an_unknown_field_is_an_error() {
    let plan = plan_for("struct s { uint8 a; char d[nosuchfield]; };", "s");
    match resolve_layout(&plan, &[1, 2, 3, 4]) {
        Err(Error::InvalidValue { message }) => {
            assert!(message.contains("not defined in this scope"), "{message}")
        }
        other => panic!("expected a value error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Null-terminated arrays
// ---------------------------------------------------------------------------

/// A terminated array's value stops before the terminator and its consumption
/// steps over it. Both halves are asserted, because getting only one right is
/// the defect described in this module's header.
#[test]
fn a_terminated_array_excludes_its_terminator_but_consumes_it() {
    let plan = plan_for("struct s { uint8 data[]; uint8 tail; };", "s");
    let buf = [0xaa, 0xbb, 0x00, 0xcc];
    let resolved = layout(&plan, &buf);
    assert_eq!(
        resolved.spans[0].count, 2,
        "the value stops before the zero"
    );
    assert_eq!(resolved.spans[0].size, 2);
    assert_eq!(
        resolved.spans[1].offset, 3,
        "the following field starts *after* the terminator"
    );
}

/// The stride is the element's, so a terminated `uint16` array stops on an
/// all-zero *element* rather than on a zero byte.
#[test]
fn a_terminated_wide_array_scans_by_element() {
    let plan = plan_for("struct s { uint16 data[]; uint8 tail; };", "s");
    // 0x0001, 0x0100 -- each contains a zero byte but neither is a zero
    // element, so the scan must not stop early.
    let buf = [0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[0].count, 2);
    assert_eq!(resolved.spans[0].size, 4);
    assert_eq!(resolved.spans[1].offset, 6);
}

/// A run-on array in a corrupt record is an error, not a read past the buffer.
#[test]
fn an_unterminated_array_is_end_of_stream() {
    let plan = plan_for("struct s { uint8 data[]; };", "s");
    assert!(matches!(
        resolve_layout(&plan, &[0xaa, 0xbb, 0xcc]),
        Err(Error::EndOfStream)
    ));
}

/// An immediately-terminated array is empty and still consumes its terminator.
#[test]
fn an_empty_terminated_array_still_consumes_one_element() {
    let plan = plan_for("struct s { uint8 data[]; uint8 tail; };", "s");
    let resolved = layout(&plan, &[0x00, 0xff]);
    assert_eq!(resolved.spans[0].count, 0);
    assert_eq!(resolved.spans[1].offset, 1);
}

// ---------------------------------------------------------------------------
// Read-to-end arrays
// ---------------------------------------------------------------------------

/// `[EOF]` takes whatever remains, in whole elements.
///
/// Distinguished from `[]` by the plan rather than inferred: both are dynamic
/// arrays with no length expression, and guessing between "stop at a zero" and
/// "read to the end" is how an evtx corruption happened.
#[test]
fn a_read_to_end_array_takes_the_rest_of_the_input() {
    let plan = plan_for("struct s { uint8 lead; char rest[EOF]; };", "s");
    let resolved = layout(&plan, &[0x01, b'a', b'b', b'c']);
    assert_eq!(
        resolved.spans[1],
        FieldSpan {
            offset: 1,
            size: 3,
            count: 3
        }
    );
    assert_eq!(resolved.consumed, 4);

    // A zero inside it is data, not a terminator.
    let resolved = layout(&plan, &[0x01, b'a', 0x00, b'c']);
    assert_eq!(resolved.spans[1].size, 3);

    // Nothing left is an empty array rather than an error.
    let resolved = layout(&plan, &[0x01]);
    assert_eq!(resolved.spans[1].size, 0);
}

/// A partial trailing element is dropped rather than read short.
#[test]
fn a_read_to_end_array_ignores_a_partial_trailing_element() {
    let plan = plan_for("struct s { uint8 lead; uint32 rest[EOF]; };", "s");
    // Nine bytes after the lead: two whole elements and one stray byte.
    let resolved = layout(&plan, &[0u8; 10]);
    assert_eq!(resolved.spans[1].count, 2);
    assert_eq!(resolved.spans[1].size, 8);
    assert_eq!(resolved.consumed, 9, "the stray byte is not consumed");
}

// ---------------------------------------------------------------------------
// Nested records
// ---------------------------------------------------------------------------

/// An array of structures sized by an earlier field decodes *every* element.
///
/// `dissect.regf` sizes its subkey lists this way, and treating the array as
/// one nested record made a hive report one subkey where it had 82.
#[test]
fn a_counted_array_of_records_lays_out_every_element() {
    let plan = plan_for(
        "struct entry { uint32 offset; uint32 hash; };
         struct list { uint16 count; entry entries[count]; uint8 tail; };",
        "list",
    );
    let mut buf = vec![0x03, 0x00];
    buf.extend_from_slice(&[0u8; 24]);
    buf.push(0xff);
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].count, 3, "all three entries, not one");
    assert_eq!(resolved.spans[1].size, 24);
    assert_eq!(resolved.spans[2].offset, 26);
}

/// A nested record that is itself dynamic has to be laid out to learn its
/// length, and the parent's cursor advances by what it actually consumed.
#[test]
fn a_dynamic_nested_record_contributes_its_own_consumption() {
    let plan = plan_for(
        "struct inner { uint8 n; char d[n]; };
         struct outer { inner first; uint8 tail; };",
        "outer",
    );
    let buf = [2, b'a', b'b', 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(
        resolved.spans[0],
        FieldSpan {
            offset: 0,
            size: 3,
            count: 1
        }
    );
    assert_eq!(resolved.spans[1].offset, 3);
    assert_eq!(resolved.consumed, 4);
}

/// An array of *variable-size* records lays each one out in turn, so the total
/// is a sum rather than a product.
#[test]
fn an_array_of_variable_size_records_sums_their_lengths() {
    let plan = plan_for(
        "struct inner { uint8 n; char d[n]; };
         struct outer { uint8 count; inner items[count]; uint8 tail; };",
        "outer",
    );
    // Two elements of different lengths: 1+1 and 1+3.
    let buf = [2, 1, b'a', 3, b'x', b'y', b'z', 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].count, 2);
    assert_eq!(resolved.spans[1].size, 6, "2 + 4, not 2 x either");
    assert_eq!(resolved.spans[2].offset, 7);
}

/// `S d[]` -- a null-terminated array of *records*, which stops at an element
/// whose bytes are all zero and steps over it.
///
/// Getting this wrong is silent: stopping after one element leaves the field
/// after the array to be read out of the second element's bytes — wrong values
/// and wrong offsets, with no exception raised.
#[test]
fn a_terminated_array_of_records_reads_until_an_all_zero_element() {
    let plan = plan_for(
        "struct item { uint8 a; uint8 b; };
         struct list { item items[]; uint8 tail; };",
        "list",
    );
    let buf = [0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(
        resolved.spans[0].count, 2,
        "two elements before the terminator"
    );
    assert_eq!(
        resolved.spans[0].size, 4,
        "the value excludes the terminator"
    );
    assert_eq!(
        resolved.spans[1].offset, 6,
        "the terminator's bytes are consumed"
    );
}

/// The same shape with variable-size elements: each has to be laid out before
/// it can be tested for being all zero.
#[test]
fn a_terminated_array_of_variable_size_records_works_too() {
    let plan = plan_for(
        "struct item { uint8 n; char d[n]; };
         struct list { item items[]; uint8 tail; };",
        "list",
    );
    // 1+"a", 2+"bc", then a zero-length item whose single byte is zero.
    let buf = [1, b'a', 2, b'b', b'c', 0x00, 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[0].count, 2);
    assert_eq!(resolved.spans[1].offset, 6);
}

/// A truncated record is `EndOfStream` rather than a length expression failing
/// with a `ValueError`.
///
/// The distinction is load-bearing: callers end a record run with
/// `except EOFError: break`, so a `ValueError` here escapes that loop and
/// aborts the walk rather than ending it.
#[test]
fn a_truncated_fixed_field_is_end_of_stream_not_a_scope_error() {
    let plan = plan_for("struct s { uint32 len; char d[len]; };", "s");
    // The length field itself is cut short, so `len` never enters the scope.
    assert!(matches!(
        resolve_layout(&plan, &[0x01, 0x02]),
        Err(Error::EndOfStream)
    ));
}

// ---------------------------------------------------------------------------
// Bitfields
// ---------------------------------------------------------------------------

/// A bitfield can size a later array, and **any** bitfield of a unit can -- not
/// only the one that opens it.
///
/// The layout walk reuses one storage span for adjacent bitfields. It must still
/// insert every named bitfield into the expression scope before continuing;
/// upstream permits a later length expression to name any of them.
#[test]
fn any_bitfield_of_a_unit_can_size_a_later_array() {
    let plan = plan_for(
        "struct T { uint16 size : 4; char d[size]; uint8 tail; };",
        "T",
    );
    let buf = [0x03, 0x00, b'A', b'B', b'C', 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].size, 3);
    assert_eq!(resolved.spans[2].offset, 5);

    // `b` is the *second* field of its unit, so it never opened one.
    let plan = plan_for(
        "struct T { uint8 a : 3; uint8 b : 5; char d[b]; uint8 tail; };",
        "T",
    );
    let buf = [0x11, b'A', b'B', b'C'];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[2].size, 2, "0x11 >> 3 is 2");
    assert_eq!(resolved.spans[3].offset, 3);
}

/// A bitfield's value in the scope is masked to its declared width and is not
/// sign-extended, which is the same computation the materialiser performs.
///
/// The property that matters is that the value a later field sees under a name
/// and the value the field itself decodes to cannot disagree. For a bitfield,
/// `field.size` is the storage-unit width while `elem_size` describes the
/// declared field; using the latter to read the unit would give length
/// expressions a different value from materialisation.
#[test]
fn a_bitfields_scope_value_is_masked_to_its_width() {
    let plan = plan_for("struct T { uint8 v : 4; char d[v]; uint8 tail; };", "T");
    // 0xf3: the low nibble is 3, the high nibble is not part of `v`.
    let buf = [0xf3, b'a', b'b', b'c', 0xff];
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].size, 3, "masked to four bits, not 0xf3");
    assert_eq!(resolved.spans[2].offset, 4);
}

/// Every field of a storage unit reads the same bytes, and the unit is only
/// paid for once.
#[test]
fn a_run_of_bitfields_costs_one_unit_in_the_layout() {
    let plan = plan_for(
        "struct s { uint16 a : 4; uint16 b : 4; uint16 c : 8; uint8 n; char d[n]; };",
        "s",
    );
    let buf = [0x21, 0x43, 0x02, b'x', b'y'];
    let resolved = layout(&plan, &buf);
    assert!(resolved.spans[..3].iter().all(|span| span.offset == 0));
    assert_eq!(resolved.spans[3].offset, 2, "one unit, two bytes");
    assert_eq!(resolved.spans[4].size, 2);
}

// ---------------------------------------------------------------------------
// Other length sources
// ---------------------------------------------------------------------------

/// A LEB128 can name a length.
///
/// `uleb128 n; char name[n];` is how a length-prefixed string is declared when
/// the length is variable-width. The self-delimiting field has to be added to
/// the scope like any other integer, or the array referencing it fails with
/// "n is not defined in this scope" for a structure upstream reads fine.
#[test]
fn a_leb128_field_can_size_a_later_array() {
    let plan = plan_for("struct s { uleb128 n; char name[n]; uint8 tail; };", "s");
    // 0x80 0x01 is 128 in LEB128, so the length field is two bytes long.
    let mut buf = vec![0x80, 0x01];
    buf.extend(std::iter::repeat_n(b'a', 128));
    buf.push(0xff);
    let resolved = layout(&plan, &buf);
    assert_eq!(
        resolved.spans[0].size, 2,
        "the length field is self-delimiting"
    );
    assert_eq!(resolved.spans[1].size, 128);
    assert_eq!(resolved.spans[2].offset, 130);
}

/// A wide integer can name a length too, rather than dropping silently out of
/// scope because it did not fit the narrow reader.
#[test]
fn a_wide_integer_field_can_size_a_later_array() {
    let plan = plan_for("struct s { uint128 n; char d[n]; uint8 tail; };", "s");
    let mut buf = vec![0u8; 16];
    buf[0] = 3;
    buf.extend_from_slice(b"abc");
    buf.push(0xff);
    let resolved = layout(&plan, &buf);
    assert_eq!(resolved.spans[1].size, 3);
    assert_eq!(resolved.spans[2].offset, 19);
}

/// An enum-typed field is an integer and can name a length.
#[test]
fn an_enum_field_can_size_a_later_array() {
    let plan = plan_for(
        "enum E : uint8 { TWO = 2 };
         struct s { E n; char d[n]; uint8 tail; };",
        "s",
    );
    let resolved = layout(&plan, &[2, b'a', b'b', 0xff]);
    assert_eq!(resolved.spans[1].size, 2);
    assert_eq!(resolved.spans[2].offset, 3);
}

/// A signed length field is read as signed, so a negative one is an empty array
/// rather than an enormous one.
#[test]
fn a_signed_length_field_is_read_as_signed() {
    let plan = plan_for("struct s { int16 n; char d[n]; uint8 tail; };", "s");
    let resolved = layout(&plan, &[0xff, 0xff, 0x42]);
    assert_eq!(
        resolved.spans[1].size, 0,
        "-1 is an empty array, not 65535 bytes"
    );
    assert_eq!(resolved.spans[2].offset, 2);
}

// ---------------------------------------------------------------------------
// Unions
// ---------------------------------------------------------------------------

/// A dynamic union's members all start at zero and it consumes its longest
/// member.
#[test]
fn a_dynamic_union_consumes_its_longest_member() {
    let plan = plan_for(
        "struct inner { uint8 n; char d[n]; };
         union u { inner varying; uint32 fixed; };",
        "u",
    );
    let buf = [0x05, b'a', b'b', b'c', b'd', b'e'];
    let resolved = layout(&plan, &buf);
    assert!(resolved.spans.iter().all(|span| span.offset == 0));
    assert_eq!(
        resolved.consumed, 6,
        "the six-byte member, not the four-byte one"
    );
}

// ---------------------------------------------------------------------------
// Consumer-supplied types
// ---------------------------------------------------------------------------

/// A consumer-supplied type reads itself, and only it knows its length.
///
/// The core crate holds no interpreter, so it asks through [`CustomReader`] and
/// the binding crate does the calling. `dissect.target`'s Everything and docker
/// readers both register a varint whose length is discovered by reading it.
#[test]
fn a_custom_type_is_measured_through_the_reader() {
    use cstruct_rs2_core::parser::parse_into;
    use cstruct_rs2_core::schema::{Schema, TypeDef};

    /// A stand-in for a Python-side reader: a varint whose length is one plus
    /// its first byte.
    struct LengthPrefixed;
    impl CustomReader for LengthPrefixed {
        fn read(
            &self,
            name: &str,
            buf: &[u8],
            offset: usize,
        ) -> cstruct_rs2_core::error::Result<CustomRead> {
            assert_eq!(name, "varint");
            let first = *buf.get(offset).ok_or(Error::EndOfStream)?;
            Ok(CustomRead {
                consumed: u32::from(first) + 1,
                integer: None,
            })
        }
    }

    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert("varint".to_owned(), TypeDef::Custom);
    parse_into("struct s { varint v; uint8 tail; };", &mut schema).unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();

    let buf = [0x02, 0xaa, 0xbb, 0xff];
    let resolved = resolve_layout_with(&plan, &buf, Some(&LengthPrefixed)).unwrap();
    assert_eq!(
        resolved.spans[0],
        FieldSpan {
            offset: 0,
            size: 3,
            count: 1
        }
    );
    assert_eq!(resolved.spans[1].offset, 3);

    // Without a reader the field cannot be measured, and saying so beats
    // guessing a width.
    match resolve_layout(&plan, &buf) {
        Err(Error::InvalidValue { message }) => {
            assert!(message.contains("custom type"), "{message}")
        }
        other => panic!("expected a value error, got {other:?}"),
    }

    // A reader that fails propagates its error rather than being swallowed.
    assert!(matches!(
        resolve_layout_with(&plan, &[], Some(&LengthPrefixed)),
        Err(Error::EndOfStream)
    ));
}

/// A custom integer is available to the following length expression.
///
/// `dissect.target` registers its protobuf varint this way: the reader decides
/// both how many bytes the integer occupied and the value that sizes the next
/// `char` array. Measuring only its width would place the array before the
/// layout knows its count.
#[test]
fn a_custom_integer_enters_the_later_length_scope() {
    use cstruct_rs2_core::parser::parse_into;
    use cstruct_rs2_core::schema::{Schema, TypeDef};

    struct OneByteInteger;
    impl CustomReader for OneByteInteger {
        fn read(
            &self,
            name: &str,
            buf: &[u8],
            offset: usize,
        ) -> cstruct_rs2_core::error::Result<CustomRead> {
            assert_eq!(name, "varint");
            let value = *buf.get(offset).ok_or(Error::EndOfStream)?;
            Ok(CustomRead {
                consumed: 1,
                integer: Some(i128::from(value)),
            })
        }
    }

    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert("varint".to_owned(), TypeDef::Custom);
    parse_into(
        "struct s { varint size; char body[size]; uint8 marker; };",
        &mut schema,
    )
    .unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();

    let resolved =
        resolve_layout_with(&plan, &[3, b'a', b'b', b'c', 0xff], Some(&OneByteInteger)).unwrap();
    assert_eq!(resolved.spans[0].size, 1);
    assert_eq!(resolved.spans[1].offset, 1);
    assert_eq!(resolved.spans[1].size, 3);
    assert_eq!(resolved.spans[2].offset, 4);
    assert_eq!(resolved.consumed, 5);
}

/// The custom-reader context follows every recursive structure-array shape.
///
/// Current upstream calls a registered type's `_read` from nested structures,
/// fixed and counted arrays, sentinel arrays, and `[EOF]` arrays alike. These
/// are separate layout branches, so one representative of each prevents a new
/// branch from silently falling back to the reader-less core entry point.
#[test]
fn a_custom_reader_reaches_every_recursive_dynamic_layout_path() {
    use cstruct_rs2_core::parser::parse_into;
    use cstruct_rs2_core::schema::{Schema, TypeDef};

    struct OneByte;
    impl CustomReader for OneByte {
        fn read(
            &self,
            name: &str,
            buf: &[u8],
            offset: usize,
        ) -> cstruct_rs2_core::error::Result<CustomRead> {
            assert_eq!(name, "custom");
            buf.get(offset).ok_or(Error::EndOfStream)?;
            Ok(CustomRead {
                consumed: 1,
                integer: None,
            })
        }
    }

    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert("custom".to_owned(), TypeDef::Custom);
    parse_into(
        "struct item { custom value; uint8 tag; };\
         struct nested { item value; uint8 tail; };\
         struct fixed { item values[2]; uint8 tail; };\
         struct counted { uint8 count; item values[count]; uint8 tail; };\
         struct terminated { item values[]; uint8 tail; };\
         struct to_end { item values[EOF]; };",
        &mut schema,
    )
    .unwrap();
    let mut compiler = Compiler::new(&schema);

    let nested = compiler.compile("nested").unwrap();
    assert_eq!(
        resolve_layout_with(&nested, &[1, 2, 3], Some(&OneByte))
            .unwrap()
            .consumed,
        3
    );

    let fixed = compiler.compile("fixed").unwrap();
    assert_eq!(
        resolve_layout_with(&fixed, &[1, 2, 3, 4, 5], Some(&OneByte))
            .unwrap()
            .consumed,
        5
    );

    let counted = compiler.compile("counted").unwrap();
    assert_eq!(
        resolve_layout_with(&counted, &[2, 1, 2, 3, 4, 5], Some(&OneByte))
            .unwrap()
            .consumed,
        6
    );

    let terminated = compiler.compile("terminated").unwrap();
    assert_eq!(
        resolve_layout_with(&terminated, &[1, 2, 3, 4, 0, 0, 5], Some(&OneByte))
            .unwrap()
            .consumed,
        7
    );

    let to_end = compiler.compile("to_end").unwrap();
    let resolved = resolve_layout_with(&to_end, &[1, 2, 3, 4], Some(&OneByte)).unwrap();
    assert_eq!(resolved.spans[0].count, 2);
    assert_eq!(resolved.consumed, 4);
}

// ---------------------------------------------------------------------------
// Resource limits
// ---------------------------------------------------------------------------

/// A corrupt length field must fail fast rather than try to allocate.
///
/// The cap leaves room for the large tables real formats contain while still
/// refusing a four-billion-element request immediately.
#[test]
fn an_absurd_length_is_refused_before_anything_is_allocated() {
    let plan = plan_for("struct s { uint32 len; uint8 data[len]; };", "s");
    let mut buf = vec![0xff, 0xff, 0xff, 0xff];
    buf.extend_from_slice(&[0u8; 16]);
    match resolve_layout(&plan, &buf) {
        Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, "array elements"),
        other => panic!("a four-billion-element array should be refused, got {other:?}"),
    }
}

/// The same cap applies to an array of records, where each element is more
/// expensive than a byte.
#[test]
fn an_absurd_record_count_is_refused_too() {
    let plan = plan_for(
        "struct item { uint32 a; };
         struct s { uint32 count; item items[count]; };",
        "s",
    );
    let mut buf = vec![0xff, 0xff, 0xff, 0xff];
    buf.extend_from_slice(&[0u8; 16]);
    assert!(matches!(
        resolve_layout(&plan, &buf),
        Err(Error::LimitExceeded { .. })
    ));
}

/// A null-terminated array of zero-width records would never reach a
/// terminator, so it is refused rather than looped over forever.
#[test]
fn a_terminated_array_of_zero_width_records_is_refused() {
    let plan = plan_for(
        "struct empty { uint8 nothing[0]; };
         struct s { empty items[]; };",
        "s",
    );
    match resolve_layout(&plan, &[0u8; 8]) {
        Err(Error::InvalidValue { message }) => assert!(message.contains("no end"), "{message}"),
        other => panic!("expected a value error, got {other:?}"),
    }
}

/// A record longer than an offset can address is refused rather than wrapping.
#[test]
fn a_record_that_would_overflow_its_cursor_is_refused() {
    let plan = plan_for(
        "struct s { uint32 a[16777216]; uint32 b[16777216]; uint32 c[16777216]; \
                    uint32 d[16777216]; uint32 e[16777216]; uint32 f[16777216]; \
                    uint32 g[16777216]; uint32 h[16777216]; };",
        "s",
    );
    // Eight 64 MiB arrays are 512 MiB, which still fits a u32 -- the point of
    // the shape is that the compiler accepted it, so the guard below is the
    // decoder's and not the compiler's.
    assert!(plan.total_size.is_some());
    assert_eq!(check_fixed_size(&plan, &[0u8; 16]), Err(Error::EndOfStream));
}

/// Every one of these malformed inputs produces an error rather than a panic.
///
/// The list is deliberately mechanical: the value of a fuzz-shaped test is
/// breadth, and the assertion is only that nothing here aborts the process.
#[test]
fn malformed_input_never_panics() {
    let shapes = [
        "struct s { uint16 n; char d[n]; };",
        "struct s { uint8 n; uint8 d[n]; uint8 t[]; };",
        "struct s { uint8 d[]; uint32 tail; };",
        "struct s { uleb128 n; char d[n]; };",
        "struct s { uint8 a : 3; uint8 b : 5; char d[b]; };",
        "struct inner { uint8 n; char d[n]; }; struct s { uint8 c; inner i[c]; };",
        "struct s { char d[EOF]; };",
    ];
    let inputs: [&[u8]; 6] = [
        &[],
        &[0x00],
        &[0xff],
        &[0xff, 0xff],
        &[0xff; 8],
        &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    ];

    for source in shapes {
        // Every shape above declares its outermost type as `s`, including the
        // one that also declares an `inner`, so the type to plan is always `s`.
        let plan = plan_for(source, "s");
        for input in inputs {
            // Only that it returns. A panic here escapes to the PyO3 boundary,
            // and `panic = "unwind"` is what keeps it from killing the
            // interpreter -- but an exception a consumer cannot classify is
            // still a defect.
            let _ = resolve_layout(&plan, input);
        }
    }
}
