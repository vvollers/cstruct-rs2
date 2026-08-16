//! Reading one primitive value out of a byte slice.
//!
//! These are the leaves of a decode. Everything above them — where a field
//! starts, how long it is, how many elements it has — is [`super::layout`]; this
//! module answers only "given a slice, an offset and a width, what is the
//! number".
//!
//! # The shape they all share
//!
//! Every reader takes `(buf, offset, size, endian)` and returns a `Result`. The
//! `size` comes from the compiled plan, never from the input, so each match is
//! exhaustive over the widths the type system can actually produce — but the
//! `offset` and the buffer length do come from the data, so every one of them
//! bounds-checks before reading. A truncated record must be [`Error::EndOfStream`]
//! and never a panic.
//!
//! # Why the widths are split at eight bytes
//!
//! `read_uint`/`read_int` handle up to 64 bits and `read_uint128`/`read_int128`
//! go wider. That split is deliberate and worth keeping: the narrow readers'
//! power-of-two arms compile to a single load, and widening them to `u128` would
//! make reading a `uint8` pay for 128-bit arithmetic. Callers branch on the
//! width, which is a constant per field.
//!
//! # Odd widths sign-extend from their own top bit
//!
//! cstruct has `int24` and `int48`, whose sign bit is not bit 63. A plain cast
//! reads every negative `int24` as a large positive number, so the odd widths
//! extend explicitly. The same reasoning applies to the wide readers.

use crate::error::{Error, Result};
use crate::schema::Endian;

/// Read an unsigned integer of `size` bytes.
///
/// `size` comes from the plan, not from the input, so the match is exhaustive
/// over the widths the type system can produce.
#[inline]
pub fn read_uint(buf: &[u8], offset: usize, size: u8, endian: Endian) -> Result<u64> {
    let end = offset
        .checked_add(size as usize)
        .ok_or(Error::EndOfStream)?;
    let bytes = buf.get(offset..end).ok_or(Error::EndOfStream)?;
    Ok(match (size, endian) {
        (1, _) => bytes[0] as u64,
        (2, Endian::Little) => u16::from_le_bytes([bytes[0], bytes[1]]) as u64,
        (2, Endian::Big) => u16::from_be_bytes([bytes[0], bytes[1]]) as u64,
        (4, Endian::Little) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
        (4, Endian::Big) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64,
        (8, Endian::Little) => u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        (8, Endian::Big) => u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        // Widths that are not a power of two: `uint24`, `uint48` and friends.
        // Definitions declare them directly — `dissect.database`'s sqlite3
        // reader is full of them — so they have to work, but a byte loop is
        // fine for shapes this rare. The power-of-two arms above are kept
        // separate because each compiles to a single load.
        (3 | 5 | 6 | 7, _) => {
            let mut value = 0u64;
            match endian {
                Endian::Little => {
                    for (index, byte) in bytes.iter().enumerate() {
                        value |= u64::from(*byte) << (8 * index);
                    }
                }
                Endian::Big => {
                    for byte in bytes {
                        value = (value << 8) | u64::from(*byte);
                    }
                }
            }
            value
        }
        _ => {
            return Err(Error::InvalidValue {
                message: format!("unsupported integer width: {size}"),
            })
        }
    })
}

/// Read a signed integer of `size` bytes, sign-extended to `i64`.
#[inline]
pub fn read_int(buf: &[u8], offset: usize, size: u8, endian: Endian) -> Result<i64> {
    let raw = read_uint(buf, offset, size, endian)?;
    Ok(match size {
        1 => raw as u8 as i8 as i64,
        2 => raw as u16 as i16 as i64,
        4 => raw as u32 as i32 as i64,
        8 => raw as i64,
        // Sign-extend from the width's own top bit. `int24`'s sign bit is bit
        // 23, not bit 63, so a plain cast would read every negative value as a
        // large positive one.
        3 | 5 | 6 | 7 => {
            let bits = size as u32 * 8;
            let sign_bit = 1u64 << (bits - 1);
            if raw & sign_bit == 0 {
                raw as i64
            } else {
                (raw as i64) - (1i64 << bits)
            }
        }
        _ => {
            return Err(Error::InvalidValue {
                message: format!("unsupported integer width: {size}"),
            })
        }
    })
}

/// Read an unsigned integer wider than 64 bits.
///
/// Kept separate from [`read_uint`] rather than widening it: `read_uint` is the
/// hot path for every `uint32` in every record, and its power-of-two arms
/// compile to a single load. Returning `u128` from it would make 128-bit
/// arithmetic the cost of reading a `uint8`. Callers branch on the width, which
/// is a constant per field.
#[inline]
pub fn read_uint128(buf: &[u8], offset: usize, size: u8, endian: Endian) -> Result<u128> {
    let end = offset
        .checked_add(size as usize)
        .ok_or(Error::EndOfStream)?;
    let bytes = buf.get(offset..end).ok_or(Error::EndOfStream)?;

    let mut value = 0u128;
    match endian {
        Endian::Little => {
            for (index, byte) in bytes.iter().enumerate() {
                value |= u128::from(*byte) << (8 * index);
            }
        }
        Endian::Big => {
            for byte in bytes {
                value = (value << 8) | u128::from(*byte);
            }
        }
    }
    Ok(value)
}

/// Read a signed integer wider than 64 bits, sign-extended from its own width.
#[inline]
pub fn read_int128(buf: &[u8], offset: usize, size: u8, endian: Endian) -> Result<i128> {
    let raw = read_uint128(buf, offset, size, endian)?;
    let bits = size as u32 * 8;
    if bits >= 128 {
        return Ok(raw as i128);
    }
    // Sign-extend from the width's own top bit, as `read_int` does for the
    // narrow odd widths. `int128`'s sign bit is bit 127; a narrower wide type
    // such as a hypothetical `int96` would sign from bit 95.
    let sign_bit = 1u128 << (bits - 1);
    Ok(if raw & sign_bit == 0 {
        raw as i128
    } else {
        (raw as i128).wrapping_sub(1i128 << bits)
    })
}

/// Read an IEEE 754 float of `size` bytes.
#[inline]
pub fn read_float(buf: &[u8], offset: usize, size: u8, endian: Endian) -> Result<f64> {
    let raw = read_uint(buf, offset, size, endian)?;
    Ok(match size {
        // binary16. Rust has no stable `f16`, so this expands the fields by
        // hand: 1 sign, 5 exponent, 10 mantissa, bias 15.
        2 => {
            let bits = raw as u16;
            let sign = if bits & 0x8000 != 0 { -1.0f64 } else { 1.0f64 };
            let exponent = ((bits >> 10) & 0x1f) as i32;
            let mantissa = (bits & 0x03ff) as f64;
            sign * match exponent {
                // Subnormal, including zero: no implicit leading one.
                0 => mantissa * 2.0f64.powi(-24),
                // Inf and NaN share the all-ones exponent.
                31 => {
                    if mantissa == 0.0 {
                        f64::INFINITY
                    } else {
                        // The sign is carried explicitly. `f64::NAN` is
                        // positive, and returning it bare turned `-nan` into
                        // `nan`, so a record holding a negative NaN
                        // re-serialised to different bytes than it was read
                        // from.
                        let sign_bit = if bits & 0x8000 != 0 { 1u64 << 63 } else { 0 };
                        return Ok(f64::from_bits(f64::NAN.to_bits() | sign_bit));
                    }
                }
                _ => (1.0 + mantissa / 1024.0) * 2.0f64.powi(exponent - 15),
            }
        }
        4 => f32::from_bits(raw as u32) as f64,
        8 => f64::from_bits(raw),
        _ => {
            return Err(Error::InvalidValue {
                message: format!("unsupported float width: {size}"),
            })
        }
    })
}

/// The inverse of `read_float`'s binary16 arm: pack an `f64` into 16 bits.
///
/// This lives beside its forward direction on purpose. It is the only place in
/// the project that spells out the binary16 field layout twice, and the two
/// spellings have to agree; splitting them across crates is how they would
/// drift. Encoding is otherwise the Python crate's job.
///
/// It is written against `f64` rather than routing through `f32` because that
/// intermediate step rounds twice, and a double rounding can land one unit in
/// the last place away from what `struct.pack("<e", x)` produces -- which is
/// the oracle rs2 is compared against.
///
/// Rounding is round-to-nearest, ties-to-even (IEEE 754 default).
///
/// A finite value too large for binary16 is an error rather than a saturation
/// to infinity. C saturates; `struct.pack("<e", 65520.0)` raises, and the
/// oracle wins -- quietly turning a number into infinity is exactly the class
/// of silent wrongness this harness exists to catch. An input that is *already*
/// infinite is not an overflow and packs normally.
pub fn float16_bits(value: f64) -> Result<u16> {
    let overflow = || Error::InvalidValue {
        message: format!("{value} does not fit in a float16"),
    };

    let bits = value.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = bits & 0x000f_ffff_ffff_ffff;

    if exponent == 0x7ff {
        // Inf and NaN share the all-ones exponent. A NaN must stay a NaN, so a
        // non-zero mantissa is carried across as binary16's quiet-NaN bit
        // rather than truncated to zero, which would turn it into infinity.
        return Ok(sign | 0x7c00 | if mantissa != 0 { 0x0200 } else { 0 });
    }

    // Rebias: f64 subtracts 1023, binary16 adds 15.
    let half_exponent = exponent - 1023 + 15;

    if half_exponent >= 0x1f {
        return Err(overflow());
    }

    if half_exponent <= 0 {
        // Subnormal territory. Every binary16 subnormal is `m * 2^-24`, so the
        // f64 significand (53 bits, implicit one restored) is shifted down to
        // that scale. Below -10 the shift exceeds the significand's width and
        // even a tie cannot round up, so the answer is a signed zero.
        if half_exponent < -10 {
            return Ok(sign);
        }
        let significand = mantissa | 0x0010_0000_0000_0000;
        let shift = (43 - half_exponent) as u32;
        let truncated = significand >> shift;
        let remainder = significand & ((1u64 << shift) - 1);
        let halfway = 1u64 << (shift - 1);
        let round_up = remainder > halfway || (remainder == halfway && truncated & 1 == 1);
        return Ok(sign | (truncated + u64::from(round_up)) as u16);
    }

    // Normal: 10 mantissa bits, so drop the low 42 of the f64's 52. A carry out
    // of the mantissa flows into the exponent field, which is what should
    // happen -- but if it carries all the way into the exponent's top bit the
    // result is infinity, and a finite input must not become one.
    let truncated = ((half_exponent as u64) << 10) | (mantissa >> 42);
    let remainder = mantissa & 0x3ff_ffff_ffff;
    let halfway = 1u64 << 41;
    let round_up = remainder > halfway || (remainder == halfway && truncated & 1 == 1);
    let packed = sign | (truncated + u64::from(round_up)) as u16;
    if packed & 0x7c00 == 0x7c00 {
        return Err(overflow());
    }
    Ok(packed)
}

/// Read a LEB128 variable-length integer, returning the value and its length.
///
/// Self-delimiting: seven value bits per byte, the top bit marking that another
/// follows. Bounded at ten bytes, which is the most a 64-bit value can need --
/// without that bound a run of `0xFF` bytes in a corrupt image scans forever.
pub fn read_leb128(buf: &[u8], offset: usize, signed: bool) -> Result<(i64, u32)> {
    const MAX_LEB128_BYTES: usize = 10;

    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    let mut length: u32 = 0;

    for index in 0..MAX_LEB128_BYTES {
        let byte = *buf.get(offset + index).ok_or(Error::Leb128EndOfStream)?;
        length += 1;
        if shift < 64 {
            value |= u64::from(byte & 0x7f) << shift;
        }
        shift += 7;
        if byte & 0x80 == 0 {
            if signed && shift < 64 && byte & 0x40 != 0 {
                // Sign-extend from the last value bit.
                value |= u64::MAX << shift;
            }
            return Ok((value as i64, length));
        }
    }

    Err(Error::InvalidValue {
        message: "LEB128 value is longer than ten bytes".to_owned(),
    })
}

/// Borrow `len` bytes at `offset`, or fail if the input is short.
#[inline]
pub fn read_bytes(buf: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    let end = offset.checked_add(len).ok_or(Error::EndOfStream)?;
    buf.get(offset..end).ok_or(Error::EndOfStream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_integers_in_both_byte_orders() {
        let buf = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(read_uint(&buf, 0, 4, Endian::Little).unwrap(), 0x04030201);
        assert_eq!(read_uint(&buf, 0, 4, Endian::Big).unwrap(), 0x01020304);
        assert_eq!(read_uint(&buf, 0, 2, Endian::Little).unwrap(), 0x0201);
    }

    /// Expected values are what Python's `struct.pack("<e", x)` produces, since
    /// that is what upstream ultimately encodes a `float16` with.
    #[test]
    fn packs_binary16_like_struct_pack() {
        for (value, want) in [
            (0.0f64, 0x0000u16),
            (-0.0, 0x8000),
            (1.0, 0x3c00),
            (-2.0, 0xc000),
            (0.5908219814300537, 0x38ba),
            // Largest finite binary16, and the first value that saturates.
            (65504.0, 0x7bff),
            (f64::INFINITY, 0x7c00),
            (f64::NEG_INFINITY, 0xfc00),
            // Smallest normal and smallest subnormal.
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
        assert!(nan & 0x03ff != 0, "a NaN must not become inf");

        // `struct.pack` raises here rather than saturating, and so does this.
        // 65519.999… still rounds down to the largest finite value; 65520 is
        // the tie that rounds up past it.
        assert!(float16_bits(65520.0).is_err());
        assert!(float16_bits(-65520.0).is_err());
        assert!(float16_bits(1e30).is_err());
        assert_eq!(float16_bits(65519.999).unwrap(), 0x7bff);
    }

    /// The pair has to agree, which is why they live next to each other.
    #[test]
    fn binary16_round_trips_through_read_float() {
        for raw in 0u16..=u16::MAX {
            let buf = raw.to_le_bytes();
            let value = read_float(&buf, 0, 2, Endian::Little).unwrap();
            let packed = float16_bits(value).expect("a decoded binary16 always re-packs");
            if value.is_nan() {
                // A NaN's payload is not preserved -- `struct.pack` does not
                // preserve it either -- but its sign must be, or a record
                // holding one re-serialises to different bytes.
                assert_eq!(packed & 0x8000, raw & 0x8000, "0x{raw:04x} lost its sign");
                assert_eq!(packed & 0x7c00, 0x7c00, "0x{raw:04x} stopped being a NaN");
                assert!(packed & 0x03ff != 0, "0x{raw:04x} became an infinity");
                continue;
            }
            // -0.0 and +0.0 decode to values that compare equal but keep their
            // sign bit, so the bit pattern is what is checked.
            assert_eq!(packed, raw, "0x{raw:04x} decoded to {value}");
        }
    }

    #[test]
    fn sign_extends_signed_integers() {
        let buf = [0xff, 0xff, 0xff, 0xff];
        assert_eq!(read_int(&buf, 0, 1, Endian::Little).unwrap(), -1);
        assert_eq!(read_int(&buf, 0, 2, Endian::Little).unwrap(), -1);
        assert_eq!(read_int(&buf, 0, 4, Endian::Little).unwrap(), -1);
    }

    /// A short buffer is `EOFError`, which `dissect` uses as control flow.
    #[test]
    fn a_short_buffer_is_end_of_stream() {
        let buf = [0x01, 0x02];
        assert_eq!(
            read_uint(&buf, 0, 4, Endian::Little),
            Err(Error::EndOfStream)
        );
        assert_eq!(read_bytes(&buf, 1, 4), Err(Error::EndOfStream));
        // And an offset past the end, rather than a length past the end.
        assert_eq!(
            read_uint(&buf, 99, 1, Endian::Little),
            Err(Error::EndOfStream)
        );
    }

    /// Offsets are attacker-controlled. Arithmetic on them must not wrap into a
    /// valid-looking range.
    #[test]
    fn offset_overflow_cannot_wrap_into_bounds() {
        let buf = [0u8; 8];
        assert_eq!(
            read_uint(&buf, usize::MAX, 4, Endian::Little),
            Err(Error::EndOfStream)
        );
        assert_eq!(read_bytes(&buf, usize::MAX, 1), Err(Error::EndOfStream));
    }
}
