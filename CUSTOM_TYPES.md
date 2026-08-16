# Custom types

## Why this guide exists

Most binary layouts can be described entirely in cstruct's C-style definition
language. Some values cannot. They may use a variable-length encoding, need
consumer-specific validation, or produce a Python object that has no built-in
cstruct type. In those cases, callers can supply a Python class that reads the
value. That class is called a custom type.

Custom types cross the boundary between consumer-owned Python code and
`cstruct-rs2`'s native layout engine. Both sides must agree on how many bytes a
reader consumes, which arguments it accepts, how arrays are handled, and what
happens when decoding fails. A mistake at that boundary can move the stream
cursor or place every following field at the wrong offset.

This guide explains how to register and implement a custom reader, use it in
nested structures and arrays, size later fields from its value, and preserve
exceptions and stream positions. It also describes the callback behavior that
differs from current upstream source and the current limitation on writing
records that contain custom fields.

`cstruct.add_custom_type()` binds a consumer-owned Python type to a name that
may then appear in C-style definitions. It is the extension point for values
whose byte width or representation cannot be expressed by the definition
language itself, such as protobuf-style variable-length integers.

The API follows current upstream source's
[`cstruct.add_custom_type()`](https://github.com/fox-it/dissect.cstruct/blob/46b850fa9cd023a9de583149caa5e9ba31254cf1/dissect/cstruct/cstruct.py#L295)
and [`BaseType` read contract](https://github.com/fox-it/dissect.cstruct/blob/46b850fa9cd023a9de583149caa5e9ba31254cf1/dissect/cstruct/types/base.py#L145).
`dissect.target` uses this extension for
[protobuf varints](https://github.com/fox-it/dissect.target/blob/92568e19bba2e2bb8bf887c7521caa7ef57b0048/dissect/target/helpers/protobuf.py#L9)
and the variable-width integers in its
[Everything database reader](https://github.com/fox-it/dissect.target/blob/92568e19bba2e2bb8bf887c7521caa7ef57b0048/dissect/target/plugins/os/windows/everything/parser.py#L24).

## Defining a reader

A portable custom reader inherits `BaseType`, implements `_read()` as a class
method, advances the supplied stream by exactly the bytes it consumes, and
returns the public Python value:

```python
from dissect.cstruct import BaseType, cstruct


class VarInt(int, BaseType):
    @classmethod
    def _read(cls, stream, *, context=None, endian=None):
        value = 0
        shift = 0
        while True:
            byte = stream.read(1)
            if not byte:
                raise EOFError("truncated variable-length integer")
            value |= (byte[0] & 0x7F) << shift
            if byte[0] & 0x80 == 0:
                return cls(value)
            shift += 7


cs = cstruct()
cs.add_custom_type("varint", VarInt)
cs.load("struct Entry { varint identifier; uint8 kind; };")

entry = cs.Entry(b"\xAC\x02\x07")
assert entry.identifier == 300
assert entry.kind == 7
```

Current upstream passes the partially decoded field dictionary as `context` and
the active byte order as `endian` from
[`Structure._read()`](https://github.com/fox-it/dissect.cstruct/blob/46b850fa9cd023a9de583149caa5e9ba31254cf1/dissect/cstruct/types/structure.py#L246).
New readers should accept both keyword arguments. This implementation currently
passes `None` for `context`, because the native layout engine does not build a
Python dictionary while it walks fields, and passes the active byte order as
`endian`. A reader that needs earlier fields should instead express that
dependency in the definition when possible.

`dissect.cstruct` 4.7 allowed readers and writers that did not accept these
keywords. Registering such a class still works: `add_custom_type()` emits the
same migration warning as current upstream and creates a namespace-local adapter
that drops only the unsupported keywords. Signature inspection happens once at
registration, not on each field read, and a `TypeError` raised inside the callback
is never mistaken for an old signature.

Current upstream calls a custom type's `_write()` when a containing record is
serialized. This implementation does not yet dispatch custom-field writes,
even when the Python class provides that method. Calling `dumps()` or `write()`
on such a record raises `NotImplementedError` rather than guessing at an
encoding. Custom types are therefore suitable only for read paths here.

## Nesting and arrays

A custom reader retains the same behavior at every structure depth. Its name is
resolved in the namespace that owns the complete compiled type, including when
the field occurs in:

- a nested dynamically sized structure;
- a fixed-count array such as `Item values[2]`;
- an array sized by an earlier field such as `Item values[count]`;
- a null-terminated structure array such as `Item values[]`; or
- an input-terminated array such as `Item values[EOF]`.

Literal count and byte width are independent. An array can have exactly two
elements even when each element consumes a different number of bytes. A
null-terminated structure array consumes its all-zero terminating record but
does not include that record in the returned list.

The Python-independent core carries a borrowed layout context through every
recursive call. The context identifies custom types by registered name because
integer indexes are local to each nested compiled plan. Python classes remain
owned by the binding layer. For a custom reader that returns an integer, the
binding passes that integer to the layout scope so a following declaration can
refer to it:

```c
struct Entry { varint length; char body[length]; };
```

This follows upstream's `Structure._read()` order: it stores the field value
before it evaluates the next field's length. A custom value that is not an
integer still decodes normally, but cannot be used as an array length.

### Replacing the whole-array reader

Upstream lets a custom scalar provide an inner class named `__ArrayType__` when
an array has meaning beyond “read this scalar repeatedly.” This implementation
uses the same protocol:

```python
from dissect.cstruct import BaseArray, BaseType


class Label(BaseType):
    @classmethod
    def _read(cls, stream, *, context=None, endian=None):
        size = stream.read(1)[0]
        return stream.read(size)

    class __ArrayType__(BaseArray):
        @classmethod
        def _read(cls, stream, *, context=None, endian=None, **kwargs):
            value = cls.__type__._read(stream, context=context, endian=endian)
            if cls.__num_entries__ == "lower":
                return value.lower()
            return value
```

After registering `Label`, a field declared as `Label name[lower]` calls this
array reader once for the whole field. The symbolic selector is available as
`__num_entries__`; fixed counts and terminated arrays use the same array-class
surface. The reader can therefore interpret a consumer-defined selector without
teaching the Rust parser what it means. Built-in arrays do not use this Python
protocol and stay on the native decode path.

## Callback and stream behavior

Variable-width decoding first determines layout and then materializes values.
The reader can therefore be invoked once to determine its byte extent and again
to construct the returned value. A custom reader must be deterministic for the
same bytes and must not rely on call count or external side effects. Current
upstream performs the corresponding work in one recursive Python read; the
separate passes here keep Python objects out of the reusable native plan.

The exception raised by `_read()` is preserved, including a consumer-defined
exception class and message. `EOFError` means that the current probe is too
short and may cause a bounded retry when the source is a seekable stream.

On successful stream decoding, the public cursor advances by exactly the
record's consumed byte count, not by any probe read-ahead. If a dynamically
sized record fails, this implementation restores the cursor to the record's
starting position so a caller can try another parser. Current upstream reads
directly from the source and can leave the cursor where a failing callback
stopped; rewind-on-failure is a deliberate recovery guarantee here.
