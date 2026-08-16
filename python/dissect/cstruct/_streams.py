"""Turning "whatever the caller passed" into something an array can read from.

Upstream's type objects accept a `bytes`, a `memoryview` or an open file, and so
do the array types here. The three helpers in this module are what the enum and
struct array wrappers use to stop caring which it was.

# Why an array needs a cursor and not a buffer

Reading *n* elements out of a plain `bytes` has to consume the buffer as it goes.
Slicing from the front each time returns element zero *n* times over — which
looks right whenever the elements happen to be identical, and is wrong, with no
error at all, as soon as they are not. So a buffer is wrapped in a `BytesIO` and
the wrapper reads through it.

Nothing here runs per record: a decode goes straight into Rust. These are for the
Python-level array wrappers, which a caller reaches deliberately.
"""

from __future__ import annotations

import io
from typing import Any


def _is_readable(value: object) -> bool:
    """Whether `value` is something to read *from* rather than a value to wrap.

    This is what lets one call mean two things: `ENUM(4)` looks up the member for
    4, and `ENUM(stream)` reads one from the stream. A buffer or anything with a
    `read` method is the second kind.
    """
    return isinstance(value, (bytes, bytearray, memoryview)) or hasattr(value, "read")


def _as_stream(source: object) -> Any:
    """Give back something with a cursor, wrapping a buffer if that is what it is.

    See the module docstring on why a bare buffer will not do.
    """
    if hasattr(source, "read"):
        return source
    return io.BytesIO(bytes(source))


def _take(source: object, size: int) -> bytes:
    """Read exactly `size` bytes, or raise.

    A short read is `EOFError`, which is what `dissect` uses to end a record run —
    returning the partial bytes instead would decode a truncated value as if it
    were whole.
    """
    raw = source.read(size) if hasattr(source, "read") else bytes(source)[:size]
    if len(raw) != size:
        raise EOFError("unexpected end of stream")
    return raw


__all__ = ["_as_stream", "_is_readable", "_take"]
