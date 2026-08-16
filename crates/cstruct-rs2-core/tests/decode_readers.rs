//! The primitive readers: turning bytes into numbers, and refusing to when the
//! bytes are not there.
//!
//! These are the smallest units in the project and every record goes through
//! them, so they are tested exhaustively where that is affordable — `binary16`
//! really does round-trip all 65,536 bit patterns below, because the pair of
//! conversions is hand-written and cheap to check completely.
//!
//! # Input is untrusted
//!
//! Offsets, widths and lengths come from the data being decoded, so each reader
//! is checked at three boundaries: input that is too short, an offset past the
//! end, and arithmetic that could wrap into a range that looks valid.
//!
//! # End of input is control flow
//!
//! Callers end a record run with `except EOFError: break`, so *which* error a
//! short buffer produces is behaviour rather than cosmetics — a `ValueError`
//! escapes that loop and aborts the walk.

use cstruct_rs2_core::decode::{
    float16_bits, read_bytes, read_float, read_int, read_int128, read_leb128, read_uint,
    read_uint128,
};
use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::schema::Endian;

// ---------------------------------------------------------------------------
// Integers
// ---------------------------------------------------------------------------

/// Every width the type system can produce, in both byte orders.
///
/// Two different code paths: the power-of-two widths compile to a single load,
/// and the odd ones (`uint24`, `uint48`) go through a byte loop. Both are
/// exercised, because formats that pack fields to odd sizes declare them
/// directly.
#[test]
fn unsigned_integers_read_at_every_width_and_byte_order() {
    let buf = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let cases = [
        (1u8, 0x01u64, 0x01u64),
        (2, 0x0201, 0x0102),
        (3, 0x030201, 0x010203),
        (4, 0x04030201, 0x01020304),
        (5, 0x0504030201, 0x0102030405),
        (6, 0x060504030201, 0x010203040506),
        (7, 0x07060504030201, 0x01020304050607),
        (8, 0x0807060504030201, 0x0102030405060708),
    ];
    for (size, little, big) in cases {
        assert_eq!(
            read_uint(&buf, 0, size, Endian::Little).unwrap(),
            little,
            "{size} LE"
        );
        assert_eq!(
            read_uint(&buf, 0, size, Endian::Big).unwrap(),
            big,
            "{size} BE"
        );
    }
}

/// A signed integer sign-extends from **its own** top bit.
///
/// `int24`'s sign bit is bit 23, not bit 63, so a plain cast reads every
/// negative value as a large positive one -- a wrong number with no error.
#[test]
fn signed_integers_extend_from_their_own_width() {
    let all_ones = [0xff; 8];
    for size in [1u8, 2, 3, 4, 5, 6, 7, 8] {
        assert_eq!(
            read_int(&all_ones, 0, size, Endian::Little).unwrap(),
            -1,
            "all-ones at width {size} is -1"
        );
    }

    // The most negative value at each odd width, which is where a mis-placed
    // sign bit shows up as a large positive number rather than as an error.
    assert_eq!(
        read_int(&[0x00, 0x00, 0x80], 0, 3, Endian::Little).unwrap(),
        -8388608
    );
    assert_eq!(
        read_int(&[0xff, 0xff, 0x7f], 0, 3, Endian::Little).unwrap(),
        8388607
    );
    assert_eq!(
        read_int(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x80], 0, 6, Endian::Little).unwrap(),
        -140737488355328
    );
    // Positive values are unaffected, in both orders.
    assert_eq!(
        read_int(&[0x01, 0x00, 0x00], 0, 3, Endian::Big).unwrap(),
        65536
    );
}

/// Widths past eight bytes go through their own readers.
///
/// They are kept separate from [`read_uint`] rather than widening it: that
/// function is the hot path for every `uint32` in every record, and returning a
/// `u128` from it would make 128-bit arithmetic the price of reading a `uint8`.
#[test]
fn wide_integers_read_through_the_128_bit_path() {
    let mut buf = [0u8; 16];
    buf[0] = 0x01;
    buf[15] = 0x80;

    assert_eq!(
        read_uint128(&buf, 0, 16, Endian::Little).unwrap(),
        (1u128 << 127) | 1
    );
    assert_eq!(
        read_uint128(&buf, 0, 16, Endian::Big).unwrap(),
        (1u128 << 120) | 0x80
    );

    // The same bytes, signed: the top bit is a sign bit and the value is
    // negative. Reading it unsigned gives a number no `i128` can hold, which is
    // exactly why `uint128` needed its own re-serialisation path.
    assert_eq!(
        read_int128(&buf, 0, 16, Endian::Little).unwrap(),
        i128::MIN + 1
    );

    // A width below 128 bits still signs from its own top bit.
    assert_eq!(
        read_int128(&[0xff; 9], 0, 9, Endian::Little).unwrap(),
        -1,
        "a nine-byte all-ones value is -1"
    );
    assert_eq!(read_int128(&[0x00; 16], 0, 16, Endian::Big).unwrap(), 0);
}

/// An unsupported width is an error naming the width, not a silent zero.
#[test]
fn an_unsupported_integer_width_is_reported() {
    let buf = [0u8; 32];
    for size in [0u8, 9, 16] {
        match read_uint(&buf, 0, size, Endian::Little) {
            Err(Error::InvalidValue { message }) => {
                assert!(message.contains("integer width"), "{size}: {message}")
            }
            other => panic!("width {size} should be refused, got {other:?}"),
        }
    }
    assert!(matches!(
        read_int(&buf, 0, 9, Endian::Little),
        Err(Error::InvalidValue { .. })
    ));
}

// ---------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------

/// binary32 and binary64 read straight from their bit patterns.
#[test]
fn single_and_double_precision_floats_read_from_their_bits() {
    assert_eq!(
        read_float(&1.0f32.to_le_bytes(), 0, 4, Endian::Little).unwrap(),
        1.0
    );
    assert_eq!(
        read_float(&1.0f32.to_be_bytes(), 0, 4, Endian::Big).unwrap(),
        1.0
    );
    assert_eq!(
        read_float(&(-0.5f64).to_le_bytes(), 0, 8, Endian::Little).unwrap(),
        -0.5
    );
    assert!(read_float(&f64::NAN.to_le_bytes(), 0, 8, Endian::Little)
        .unwrap()
        .is_nan());
    match read_float(&[0u8; 8], 0, 3, Endian::Little) {
        Err(Error::InvalidValue { message }) => {
            assert!(message.contains("float width"), "{message}")
        }
        other => panic!("a three-byte float should be refused, got {other:?}"),
    }
}

/// binary16, which Rust has no stable type for and this crate expands by hand.
///
/// The named cases are the ones a hand-rolled implementation gets wrong:
/// subnormals (no implicit leading one), the two infinities, and a NaN's sign.
/// `-nan` losing its sign was a real defect -- a record holding one
/// re-serialised to different bytes than it was read from.
#[test]
fn binary16_covers_subnormals_infinities_and_signed_nan() {
    let read = |bits: u16| read_float(&bits.to_le_bytes(), 0, 2, Endian::Little).unwrap();

    assert_eq!(read(0x0000), 0.0);
    assert_eq!(read(0x3c00), 1.0);
    assert_eq!(read(0xc000), -2.0);
    assert_eq!(read(0x7bff), 65504.0, "the largest finite binary16");
    assert_eq!(read(0x0400), 6.103515625e-05, "the smallest normal");
    assert_eq!(
        read(0x0001),
        5.960464477539063e-08,
        "the smallest subnormal"
    );
    assert_eq!(read(0x7c00), f64::INFINITY);
    assert_eq!(read(0xfc00), f64::NEG_INFINITY);

    assert!(read(0x7e00).is_nan());
    assert!(read(0xfe00).is_nan());
    assert!(
        read(0xfe00).is_sign_negative(),
        "a negative NaN must stay negative"
    );

    // -0.0 compares equal to 0.0, so the sign has to be checked on the bits.
    assert!(read(0x8000).is_sign_negative(), "-0.0 keeps its sign");
}

/// Packing binary16 matches `struct.pack("<e", x)`, which is the oracle.
///
/// Written against `f64` rather than routing through `f32`, because that
/// intermediate step rounds twice and a double rounding can land one unit in
/// the last place away from the oracle.
#[test]
fn packing_binary16_matches_struct_pack() {
    for (value, want) in [
        (0.0f64, 0x0000u16),
        (-0.0, 0x8000),
        (1.0, 0x3c00),
        (-2.0, 0xc000),
        (0.5908219814300537, 0x38ba),
        (65504.0, 0x7bff),
        (f64::INFINITY, 0x7c00),
        (f64::NEG_INFINITY, 0xfc00),
        (6.103515625e-05, 0x0400),
        (5.960464477539063e-08, 0x0001),
        // Ties-to-even, not ties-away: exactly half a subnormal ULP.
        (2.9802322387695312e-08, 0x0000),
        // Below anything representable: a signed zero, not a NaN.
        (1e-30, 0x0000),
        (-1e-30, 0x8000),
    ] {
        assert_eq!(float16_bits(value).unwrap(), want, "packing {value}");
    }

    let nan = float16_bits(f64::NAN).unwrap();
    assert_eq!(nan & 0x7c00, 0x7c00);
    assert_ne!(nan & 0x03ff, 0, "a NaN must not become an infinity");
}

/// A finite value too large for binary16 is an error rather than a saturation
/// to infinity.
///
/// C saturates here; Python's `struct.pack("<e", 65520.0)` raises, and upstream
/// encodes through `struct`. Turning a finite number into an infinity without
/// saying so would be a wrong value with nothing to notice it by.
///
/// An input that is *already* infinite is not an overflow and packs normally.
#[test]
fn a_finite_value_too_large_for_binary16_is_refused() {
    for value in [65520.0f64, -65520.0, 1e30, -1e30, f64::MAX] {
        assert!(
            float16_bits(value).is_err(),
            "{value} does not fit and must not become an infinity"
        );
    }
    // 65519.999… still rounds down to the largest finite value; 65520 is the
    // tie that rounds up past it.
    assert_eq!(float16_bits(65519.999).unwrap(), 0x7bff);
    assert!(float16_bits(f64::INFINITY).is_ok());
}

/// The two binary16 directions have to agree exactly, so both are checked
/// against every one of the 65,536 possible patterns. Rust has no native
/// `f16`, so both are written out by hand — which is why an exhaustive check is
/// worth its cost here and would not be for a native type.
#[test]
fn binary16_round_trips_every_bit_pattern() {
    for raw in 0u16..=u16::MAX {
        let value = read_float(&raw.to_le_bytes(), 0, 2, Endian::Little).unwrap();
        let packed = float16_bits(value).expect("a decoded binary16 always re-packs");
        if value.is_nan() {
            // A NaN's payload is not preserved -- `struct.pack` does not
            // preserve it either -- but its sign must be, or a record holding
            // one re-serialises to different bytes than it was read from.
            assert_eq!(packed & 0x8000, raw & 0x8000, "0x{raw:04x} lost its sign");
            assert_eq!(packed & 0x7c00, 0x7c00, "0x{raw:04x} stopped being a NaN");
            assert_ne!(packed & 0x03ff, 0, "0x{raw:04x} became an infinity");
            continue;
        }
        // -0.0 and +0.0 compare equal but keep their sign bit, so the bit
        // pattern is what is checked rather than the value.
        assert_eq!(packed, raw, "0x{raw:04x} decoded to {value}");
    }
}

// ---------------------------------------------------------------------------
// LEB128
// ---------------------------------------------------------------------------

/// Seven value bits per byte, the top bit marking that another follows.
///
/// Windows Notepad's tab format writes every string length this way, and
/// `dissect.target`'s reader depends on it.
#[test]
fn leb128_reads_its_own_length() {
    for (bytes, want, length) in [
        (vec![0x00], 0i64, 1u32),
        (vec![0x7f], 127, 1),
        (vec![0x80, 0x01], 128, 2),
        (vec![0xe5, 0x8e, 0x26], 624485, 3),
    ] {
        assert_eq!(
            read_leb128(&bytes, 0, false).unwrap(),
            (want, length),
            "{bytes:02x?}"
        );
    }
    // Reading from an offset, with trailing bytes that are not part of it.
    assert_eq!(
        read_leb128(&[0xff, 0x80, 0x01, 0xff], 1, false).unwrap(),
        (128, 2)
    );
}

/// A signed LEB128 sign-extends from its last value bit.
#[test]
fn signed_leb128_extends_its_sign() {
    assert_eq!(read_leb128(&[0x7f], 0, true).unwrap(), (-1, 1));
    assert_eq!(read_leb128(&[0x40], 0, true).unwrap(), (-64, 1));
    assert_eq!(read_leb128(&[0x3f], 0, true).unwrap(), (63, 1));
    assert_eq!(
        read_leb128(&[0xc0, 0xbb, 0x78], 0, true).unwrap(),
        (-123456, 3)
    );
    // The same bytes read unsigned are a different, positive number -- which is
    // why signedness is carried on the field rather than guessed.
    assert_eq!(read_leb128(&[0x7f], 0, false).unwrap(), (127, 1));
}

/// A run of continuation bytes in a corrupt image must terminate.
///
/// Ten bytes is the most a 64-bit value can need; without the bound a run of
/// `0xFF` scans until it falls off the buffer or forever.
#[test]
fn an_overlong_leb128_is_refused() {
    match read_leb128(&[0xff; 16], 0, false) {
        Err(Error::InvalidValue { message }) => assert!(message.contains("ten bytes"), "{message}"),
        other => panic!("an overlong LEB128 should be refused, got {other:?}"),
    }
    // Truncated: the continuation bit says more is coming and the input ends.
    assert_eq!(
        read_leb128(&[0x80, 0x80], 0, false),
        Err(Error::Leb128EndOfStream)
    );
    assert_eq!(read_leb128(&[], 0, false), Err(Error::Leb128EndOfStream));
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// A short buffer is `EndOfStream`, which maps to the `EOFError` that `dissect`
/// uses as loop control.
#[test]
fn a_short_buffer_is_end_of_stream_everywhere() {
    let buf = [0x01, 0x02];
    assert_eq!(
        read_uint(&buf, 0, 4, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_int(&buf, 0, 8, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_uint128(&buf, 0, 16, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_int128(&buf, 0, 16, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_float(&buf, 0, 8, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(read_bytes(&buf, 1, 4), Err(Error::EndOfStream));
    // An offset past the end, rather than a length past the end.
    assert_eq!(
        read_uint(&buf, 99, 1, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(read_bytes(&buf, 99, 0), Err(Error::EndOfStream));
}

/// Offset arithmetic must not wrap into a valid-looking range.
///
/// Offsets are attacker-controlled: `offset + size` overflowing back into the
/// buffer's bounds is how a length check is turned into an out-of-bounds read.
#[test]
fn offset_overflow_cannot_wrap_into_bounds() {
    let buf = [0u8; 8];
    assert_eq!(
        read_uint(&buf, usize::MAX, 4, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_uint128(&buf, usize::MAX, 16, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(read_bytes(&buf, usize::MAX, 1), Err(Error::EndOfStream));
    assert_eq!(read_bytes(&buf, 1, usize::MAX), Err(Error::EndOfStream));
}

/// Reading exactly to the end of a buffer succeeds; one byte more does not.
///
/// The off-by-one at the boundary is the interesting case, and it is the one a
/// bounds check gets wrong.
#[test]
fn the_boundary_is_inclusive_at_the_end_of_the_buffer() {
    let buf = [0xaa, 0xbb, 0xcc, 0xdd];
    assert_eq!(read_uint(&buf, 0, 4, Endian::Little).unwrap(), 0xddccbbaa);
    assert_eq!(read_uint(&buf, 3, 1, Endian::Little).unwrap(), 0xdd);
    assert_eq!(
        read_uint(&buf, 4, 1, Endian::Little),
        Err(Error::EndOfStream)
    );
    assert_eq!(
        read_bytes(&buf, 4, 0).unwrap(),
        b"",
        "an empty read at the end"
    );
    assert_eq!(read_bytes(&buf, 0, 4).unwrap(), &buf);
    assert_eq!(read_bytes(&buf, 5, 0), Err(Error::EndOfStream));
}

/// Borrowing bytes is a borrow, not a copy -- the whole reason the binding
/// layer can hand a slice straight to CPython without allocating.
#[test]
fn read_bytes_borrows_from_the_input() {
    let buf = [1u8, 2, 3, 4, 5];
    let slice = read_bytes(&buf, 1, 3).unwrap();
    assert_eq!(slice, &[2, 3, 4]);
    // Compared as addresses rather than by pointer arithmetic, because this
    // crate forbids `unsafe` and that rule applies to its tests as well.
    assert_eq!(
        slice.as_ptr() as usize - buf.as_ptr() as usize,
        1,
        "the slice must point into the input, not into a copy"
    );
}
