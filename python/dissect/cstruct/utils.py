"""Byte-packing and diagnostic helpers.

``dissect.cstruct.utils`` is the name the installed ecosystem imports
(``dissect.target.tools.qfind``); ``dissect.cstruct.util`` is the 5.0 spelling
and re-exports from here. Both must yield the same objects.

Nothing here runs while decoding: these are helpers a caller invokes directly.
That is why they are Python rather than Rust.

# Sizes are in bits

``pack(1, 24) == b"\\x01\\x00\\x00"``. The ``size`` argument counts bits, not
bytes, which is what makes ``pack(value, 24)`` mean a 24-bit field. It reads
like a byte count and is not one.
"""

from __future__ import annotations

import struct

from dissect.cstruct import (
    COLOR_BG_BLUE,
    COLOR_BG_CYAN,
    COLOR_BG_GREEN,
    COLOR_BG_PURPLE,
    COLOR_BG_RED,
    COLOR_BG_WHITE,
    COLOR_BG_YELLOW,
    COLOR_BLACK,
    COLOR_BLACK_BOLD,
    COLOR_BLUE,
    COLOR_BLUE_BOLD,
    COLOR_CLEAR,
    COLOR_CLEAR_BOLD,
    COLOR_CYAN,
    COLOR_CYAN_BOLD,
    COLOR_GREEN,
    COLOR_GREEN_BOLD,
    COLOR_GREY,
    COLOR_GREY_BOLD,
    COLOR_PURPLE,
    COLOR_PURPLE_BOLD,
    COLOR_RED,
    COLOR_RED_BOLD,
    COLOR_WHITE,
    COLOR_WHITE_BOLD,
    COLOR_YELLOW,
    COLOR_YELLOW_BOLD,
    PRINTABLE,
    dumpstruct,
    hexdump,
)

#: Accepted spellings, mapped to a `struct` byte-order character. "network" is
#: big-endian by definition (RFC 1700); upstream accepts it as an alias.
_ENDIAN = {
    "little": "<",
    "big": ">",
    "network": ">",
    "<": "<",
    ">": ">",
    "!": ">",
    "=": "=",
    "@": "@",
}


def normalize_endianness(endian: str) -> str:
    """Map an endianness spelling to a :mod:`struct` byte-order character."""
    try:
        return _ENDIAN[endian]
    except KeyError:
        raise ValueError(f"invalid endianness: {endian!r}") from None


def _byte_order(endian: str) -> str:
    return "little" if normalize_endianness(endian) == "<" else "big"


def pack(value: int, size: int | None = None, endian: str = "little") -> bytes:
    """Pack an integer into bytes.

    ``size`` is a width in **bits**; when omitted the smallest whole number of
    bytes that holds ``value`` is used.
    """
    if size is None:
        length = max(1, (value.bit_length() + 7) // 8)
        if value < 0:
            length = max(1, (value.bit_length() + 8) // 8)
    else:
        length = size // 8

    return value.to_bytes(length, _byte_order(endian), signed=value < 0)


def unpack(
    value: bytes, size: int | None = None, endian: str = "little", sign: bool = False
) -> int:
    """Unpack bytes into an integer. ``size`` is a width in **bits**."""
    if size is not None:
        value = value[: size // 8]
    return int.from_bytes(value, _byte_order(endian), signed=sign)


def _pack_int(value: int, bits: int, endian: str) -> bytes:
    return value.to_bytes(bits // 8, _byte_order(endian), signed=value < 0)


def _unpack_int(value: bytes, bits: int, endian: str, sign: bool) -> int:
    return int.from_bytes(value[: bits // 8], _byte_order(endian), signed=sign)


def p8(value: int, endian: str = "little") -> bytes:
    """Pack an 8-bit integer."""
    return _pack_int(value, 8, endian)


def p16(value: int, endian: str = "little") -> bytes:
    """Pack a 16-bit integer."""
    return _pack_int(value, 16, endian)


def p32(value: int, endian: str = "little") -> bytes:
    """Pack a 32-bit integer."""
    return _pack_int(value, 32, endian)


def p64(value: int, endian: str = "little") -> bytes:
    """Pack a 64-bit integer."""
    return _pack_int(value, 64, endian)


def u8(value: bytes, endian: str = "little", sign: bool = False) -> int:
    """Unpack an 8-bit integer."""
    return _unpack_int(value, 8, endian, sign)


def u16(value: bytes, endian: str = "little", sign: bool = False) -> int:
    """Unpack a 16-bit integer."""
    return _unpack_int(value, 16, endian, sign)


def u32(value: bytes, endian: str = "little", sign: bool = False) -> int:
    """Unpack a 32-bit integer."""
    return _unpack_int(value, 32, endian, sign)


def u64(value: bytes, endian: str = "little", sign: bool = False) -> int:
    """Unpack a 64-bit integer."""
    return _unpack_int(value, 64, endian, sign)


def pf16(value: float, endian: str = "little") -> bytes:
    """Pack a half-precision float."""
    return struct.pack(f"{normalize_endianness(endian)}e", value)


def pf32(value: float, endian: str = "little") -> bytes:
    """Pack a single-precision float."""
    return struct.pack(f"{normalize_endianness(endian)}f", value)


def pf64(value: float, endian: str = "little") -> bytes:
    """Pack a double-precision float."""
    return struct.pack(f"{normalize_endianness(endian)}d", value)


def f16(value: bytes, endian: str = "little") -> float:
    """Unpack a half-precision float."""
    return struct.unpack(f"{normalize_endianness(endian)}e", value[:2])[0]


def f32(value: bytes, endian: str = "little") -> float:
    """Unpack a single-precision float."""
    return struct.unpack(f"{normalize_endianness(endian)}f", value[:4])[0]


def f64(value: bytes, endian: str = "little") -> float:
    """Unpack a double-precision float."""
    return struct.unpack(f"{normalize_endianness(endian)}d", value[:8])[0]


def swap(value: int, size: int) -> int:
    """Reverse the byte order of an integer of ``size`` **bits**."""
    length = size // 8
    return int.from_bytes(value.to_bytes(length, "little"), "big")


def swap16(value: int) -> int:
    """Reverse the byte order of a 16-bit integer."""
    return swap(value, 16)


def swap32(value: int) -> int:
    """Reverse the byte order of a 32-bit integer."""
    return swap(value, 32)


def swap64(value: int) -> int:
    """Reverse the byte order of a 64-bit integer."""
    return swap(value, 64)


__all__ = [
    "COLOR_BG_BLUE",
    "COLOR_BG_CYAN",
    "COLOR_BG_GREEN",
    "COLOR_BG_PURPLE",
    "COLOR_BG_RED",
    "COLOR_BG_WHITE",
    "COLOR_BG_YELLOW",
    "COLOR_BLACK",
    "COLOR_BLACK_BOLD",
    "COLOR_BLUE",
    "COLOR_BLUE_BOLD",
    "COLOR_CLEAR",
    "COLOR_CLEAR_BOLD",
    "COLOR_CYAN",
    "COLOR_CYAN_BOLD",
    "COLOR_GREEN",
    "COLOR_GREEN_BOLD",
    "COLOR_GREY",
    "COLOR_GREY_BOLD",
    "COLOR_PURPLE",
    "COLOR_PURPLE_BOLD",
    "COLOR_RED",
    "COLOR_RED_BOLD",
    "COLOR_WHITE",
    "COLOR_WHITE_BOLD",
    "COLOR_YELLOW",
    "COLOR_YELLOW_BOLD",
    "PRINTABLE",
    "dumpstruct",
    "f16",
    "f32",
    "f64",
    "hexdump",
    "normalize_endianness",
    "p8",
    "p16",
    "p32",
    "p64",
    "pack",
    "pf16",
    "pf32",
    "pf64",
    "swap",
    "swap16",
    "swap32",
    "swap64",
    "u8",
    "u16",
    "u32",
    "u64",
    "unpack",
]
