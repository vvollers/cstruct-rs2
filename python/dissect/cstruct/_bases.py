"""Python type protocol used by consumer-defined cstruct types.

Built-in scalars and structures are native objects, but Dissect plugins may
subclass :class:`BaseType` and provide ``_read`` and ``_write`` methods.  These
classes follow upstream's metaclass protocol so they can be called with bytes,
subscripted into arrays, and bound to one :class:`cstruct` namespace.

Only consumer-defined types use this module.  Built-in arrays remain native and
therefore do not pay for Python subclass or callback checks while decoding.
"""

from __future__ import annotations

from io import BytesIO
from typing import Any, BinaryIO


def _is_buffer(value: object) -> bool:
    """Return whether *value* can be read without calling a stream method."""
    return isinstance(value, (bytes, bytearray, memoryview))


class MetaType(type):
    """Metaclass for a consumer-defined cstruct type.

    Upstream uses the metaclass to distinguish ``Type(value)`` from
    ``Type(bytes_or_stream)``.  Keeping that decision here lets the native
    custom-field operation make one callback without wrapping ordinary values.
    """

    def __call__(cls, *args: Any, **kwargs: Any) -> Any:
        if len(args) == 1 and not isinstance(args[0], cls) and hasattr(cls, "_read"):
            source = args[0]
            if _is_buffer(source):
                return cls._read(
                    BytesIO(source),
                    context=None,
                    endian=kwargs.get("endian", cls.__cs__.endian),
                )
            if hasattr(source, "read"):
                return cls._read(
                    source,
                    context=None,
                    endian=kwargs.get("endian", cls.__cs__.endian),
                )
        return type.__call__(cls, *args, **kwargs)

    def __getitem__(cls, count: Any) -> type[BaseArray]:
        """Return this type's namespace-bound array class.

        Pointer classes need to preserve ``Pointer[T]`` for annotations while
        also accepting an integer array count.  They opt into that exceptional
        dispatch with ``_cstruct_getitem``; ordinary custom types follow
        upstream's array factory directly.
        """
        custom_getitem = getattr(cls, "_cstruct_getitem", None)
        if custom_getitem is not None:
            return custom_getitem(count)
        return cls.__cs__._make_array(cls, count)

    def __bool__(cls) -> bool:
        return True

    def __len__(cls) -> int:
        if cls.__size__ is None:
            raise TypeError("Dynamic size")
        return cls.__size__

    def __default__(cls) -> Any:
        return cls()

    def reads(cls, data: bytes | bytearray | memoryview, *, endian: str | None = None) -> Any:
        return cls._read(
            BytesIO(data),
            context=None,
            endian=endian or cls.__cs__.endian,
        )

    def read(
        cls,
        source: BinaryIO | bytes | bytearray | memoryview,
        *,
        endian: str | None = None,
    ) -> Any:
        if _is_buffer(source):
            return cls.reads(source, endian=endian)
        if not hasattr(source, "read"):
            raise TypeError("Invalid object type")
        return cls._read(source, context=None, endian=endian or cls.__cs__.endian)

    def dumps(cls, value: Any, *, endian: str | None = None) -> bytes:
        stream = BytesIO()
        cls._write(stream, value, endian=endian or cls.__cs__.endian)
        return stream.getvalue()

    def write(cls, stream: BinaryIO, value: Any, *, endian: str | None = None) -> int:
        return cls._write(stream, value, endian=endian or cls.__cs__.endian)


class BaseType(metaclass=MetaType):
    """Base class for types whose reader and writer are supplied by a plugin."""

    __ArrayType__: type[BaseArray]

    def __len__(self) -> int:
        size = self.__class__.__size__
        if size is None:
            raise TypeError("Dynamic size")
        return size


class BaseArray(BaseType):
    """Base for a custom type's fixed, expression-sized, or terminated array.

    A custom type may subclass this and override ``_read``.  That override is
    invoked once for the whole field, matching upstream and avoiding a
    field-by-field boundary crossing for built-in arrays.
    """

    __type__: type[BaseType]
    __num_entries__: Any
    __null_terminated__: bool

    @classmethod
    def __default__(cls) -> Any:
        count = cls.__num_entries__ if isinstance(cls.__num_entries__, int) else 0
        return type.__call__(cls, [cls.__type__.__default__()] * max(0, count))

    @classmethod
    def _read(
        cls,
        stream: BinaryIO,
        *,
        context: dict[str, Any] | None = None,
        endian: str,
        **kwargs: Any,
    ) -> Any:
        del kwargs
        if cls.__null_terminated__:
            return cls.__type__._read_0(stream, context=context, endian=endian)
        if isinstance(cls.__num_entries__, int):
            return cls.__type__._read_array(
                stream,
                max(0, cls.__num_entries__),
                context=context,
                endian=endian,
            )
        # A custom array subclass may attach meaning to a symbolic selector.
        # The base cannot safely guess that meaning.
        raise ValueError(f"Cannot evaluate custom array size {cls.__num_entries__!r}")


class Array(list[Any], BaseArray):
    """Default concrete value class for consumer-defined arrays."""

    @classmethod
    def _read(
        cls,
        stream: BinaryIO,
        *,
        context: dict[str, Any] | None = None,
        endian: str,
        **kwargs: Any,
    ) -> Array:
        return cls(super()._read(stream, context=context, endian=endian, **kwargs))


class NativeArrayAlias(Array):
    """Cold class surface for an array declared by a typedef.

    Current upstream publishes every typedef as a Python type, including
    ``typedef char uuid_t[16]``. Built-in rs2 arrays are native type objects for
    fast standalone and field reads, so they cannot themselves become Python
    base classes. This small class delegates each explicit typedef call to that
    native object. Ordinary structure fields continue to use their compiled
    native array operation and never pass through this class.
    """

    __native_type__: Any

    @classmethod
    def __default__(cls) -> Any:
        return cls.__native_type__.__default__()

    @classmethod
    def _read(
        cls,
        stream: BinaryIO,
        *,
        context: dict[str, Any] | None = None,
        endian: str,
        **kwargs: Any,
    ) -> Any:
        del context, endian, kwargs
        return cls.__native_type__.read(stream)

    @classmethod
    def _write(
        cls,
        stream: BinaryIO,
        value: Any,
        *,
        endian: str,
        **kwargs: Any,
    ) -> int:
        del kwargs
        return cls.__native_type__.write(stream, value, endian=endian)


BaseType.__ArrayType__ = Array

__all__ = ["Array", "BaseArray", "BaseType", "MetaType", "NativeArrayAlias"]
