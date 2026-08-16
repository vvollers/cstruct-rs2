"""``Pointer``: an integer that addresses another type.

# What a pointer is here

A pointer *value* is a pointer-sized integer, and rs2 decodes it as one. What
this class adds is the two things an integer cannot carry: the type being
addressed, and the stream the address is into. With both, ``dereference()`` can
seek and read; without them there is nothing to follow, because a forensic image
is not an address space and an offset only means something relative to the
buffer it came from.

The pointer *width* belongs to the namespace, not to the definition:
``cs.pointer = cs.uint16`` and ``cstruct(pointer="uint16")`` both set it, and it
is read live from ``__cs__.pointer`` rather than captured, so re-pointing a
namespace changes how its existing pointer types read. That is upstream's
behaviour and it is the opposite of how rs2's *structure* types work, where the
layout is fixed at definition time — the difference being that a pointer type is
not a schema entry and has no compiled plan to fix.

# Structure fields

``uint32 *ptr;`` decodes to one of these, not to a bare ``int``. The decoder
builds the class once per plan and constructs an instance per decoded pointer,
so ``record.ptr.dereference()`` works and ``isinstance(record.ptr, Pointer)`` is
true. Because ``Pointer`` subclasses ``int``, every consumer that treated the
field as a number still does.

The target type is resolved *lazily* -- see ``Pointer._target``. That is what
makes ``struct Node { Node *next; }`` possible: the pointer class is built while
``Node`` itself is being compiled, so asking for ``Node`` at that moment would
recurse forever.
"""

from __future__ import annotations

import io
from functools import cache, partial
from typing import TYPE_CHECKING, Any, Generic, TypeVar

from dissect.cstruct import BaseType
from dissect.cstruct.exception import NullPointerDereference

if TYPE_CHECKING:
    from collections.abc import Callable

T = TypeVar("T", bound=BaseType)


def _as_stream(source: Any) -> Any:
    """Give back something seekable, wrapping a buffer if that is what it is."""
    if source is None or hasattr(source, "seek"):
        return source
    return io.BytesIO(source)


def _reads_bytes(target: Any) -> bool:
    """Whether a target type is `char`, which a pointer treats as a string.

    Upstream special-cases this: a `char *` addresses a null-terminated string,
    not a single byte. Detected by what the type *is* rather than by name, so an
    alias reaches the same branch.
    """
    return getattr(target, "name", None) == "char" or getattr(target, "__name__", None) == "char"


def _pointer_dumps(pointer_type, value, *, endian: str | None = None) -> bytes:
    """Encode an address through the namespace's live pointer integer type."""
    return pointer_type.__ptr_type__().dumps(int(value), endian=endian)


def _pointer_write(pointer_type, stream, value, *, endian: str | None = None) -> int:
    """Write one address and return the sink's reported byte count."""
    return stream.write(_pointer_dumps(pointer_type, value, endian=endian))


class _PointerOverload:
    """Bind one writer for both pointer classes and pointer instances."""

    def __init__(self, function):
        self.function = function

    def __get__(self, instance, owner):
        if instance is None:
            return partial(self.function, owner)
        return partial(self.function, owner, value=instance)


class PointerArray(list):
    """What `ptr[n]` produces: a fixed- or variable-length run of pointers."""

    #: The pointer type each element has.
    __type__: type[Pointer]
    #: Element count, or None for a variable-length array.
    __count__: int | str | None = None

    @classmethod
    def __default__(cls) -> PointerArray:
        count = cls.__count__ if isinstance(cls.__count__, int) else 0
        return cls(cls.__type__.__default__() for _ in range(count))


class _LazyPointerTarget:
    """Resolve a named pointer target after its enclosing type exists.

    A self-referential structure creates its pointer class while the structure
    itself is still being compiled. The descriptor postpones only the target
    lookup; once read, it replaces itself with the real type object just as
    upstream stores that object on the pointer class.
    """

    def __init__(self, name: str):
        self.name = name

    def __get__(self, instance: object, owner: type) -> Any:
        target = getattr(owner.__cs__, self.name)
        owner.__type__ = target
        return target


class Pointer(int, BaseType, Generic[T]):  # noqa: UP046
    """An address of another type, stored as an integer.

    Subclasses are built by ``cstruct._make_pointer`` and carry either
    ``__type__`` (the addressed type) or ``__type_name__`` (its name, resolved
    on first use), plus ``__cs__`` -- the namespace, which owns the width.

    ``Generic[T]`` rather than the PEP 695 ``class Pointer[T]`` spelling, which
    is what the linter would prefer. The two are not interchangeable here:
    ``_make_pointer`` builds subclasses with a three-argument ``type()`` call, and
    the PEP 695 form attaches ``__type_params__`` machinery that a dynamically
    built subclass does not go through. Upstream uses ``Generic`` too, so this
    also keeps the class's shape matching what a consumer sees there.
    """

    #: The type this pointer addresses. Resolved lazily; see `_target`.
    __type__: type[T]
    #: The *name* of the addressed type, when the class was built from one.
    __type_name__: str | None = None
    #: The namespace this pointer belongs to; its `pointer` gives the width.
    __cs__: Any = None

    dumps = _PointerOverload(_pointer_dumps)
    write = _PointerOverload(_pointer_write)

    @classmethod
    def _target(cls) -> Any:
        """The addressed type, resolved on first use.

        Lazy on purpose, and not for speed. A pointer class is built while its
        *enclosing* type is being compiled, and `struct Node { Node *next; }`
        is the ordinary shape for a linked list -- resolving the target eagerly
        would ask the namespace for `Node` in the middle of building `Node` and
        recurse until the stack ran out. Nothing needs the target until someone
        dereferences, by which time the type exists.
        """
        return cls.__type__

    def __new__(
        cls,
        value: Any = 0,
        stream: Any = None,
        *,
        context: Any = None,
        endian: str | None = None,
        **kwargs: Any,
    ) -> Pointer:
        """Build a pointer from an address, or read one from bytes or a stream.

        Both spellings are in upstream's suite and both are load-bearing:
        ``ptr(b"\\x01\\xff")`` reads the address *out of* the buffer and keeps
        that buffer as what the address points into, while ``ptr(0, None)``
        states an address directly. The first argument tells them apart — an
        integer is an address, anything else is something to read from.
        """
        if isinstance(value, int) and not isinstance(value, bool):
            address = int(value)
            source = _as_stream(stream)
        else:
            source = _as_stream(value)
            reader = cls.__ptr_type__()
            # Read the address, then rewind: the address points into the same
            # buffer it was read from, so leaving the cursor past it would make
            # every later `dereference()` measure from the wrong place.
            start = source.tell()
            address = int(reader(source, **({"endian": endian} if endian else {})))
            source.seek(start)

        obj = super().__new__(cls, address)
        obj.__stream = source
        obj.__context = context
        obj.__endian = endian
        # Current upstream memoizes a successful dereference on each pointer.
        # `functools.cache` does not retain exceptions, so a caller can repair
        # or extend a failed source and try again.
        obj.dereference = cache(obj.dereference)
        return obj

    @classmethod
    def __ptr_type__(cls) -> Any:
        """The integer type the address itself is stored as.

        Read live from the namespace rather than captured at class creation:
        upstream consults `cs.pointer` on every read and write, so
        `cs.pointer = cs.uint16` retroactively changes existing pointer types.
        """
        namespace = getattr(cls, "__cs__", None)
        if namespace is None:
            raise TypeError("this pointer type is not attached to a cstruct namespace")
        return namespace.pointer

    @classmethod
    def __default__(cls) -> Pointer:
        """A null pointer, which is what an all-zero field decodes to."""
        return cls(0, None)

    def __class_getitem__(cls, item: Any) -> Any:
        """`ptr[4]` and `ptr[None]` are array types, not generic parameters.

        `Pointer` is `Generic[T]`, so the subscript syntax is already taken by
        typing. An integer, a symbolic length string or ``None`` means an array
        of pointers — the spelling every other cstruct type uses. Type objects
        are left to `Generic`, which is what `Pointer[SomeType]` in an
        annotation needs.
        """
        if item is None or isinstance(item, (int, str)):
            return type(
                f"{cls.__name__}[{'' if item is None else item}]",
                (PointerArray,),
                {"__type__": cls, "__count__": item},
            )
        return super().__class_getitem__(item)

    @classmethod
    def _cstruct_getitem(cls, item: Any) -> Any:
        """Preserve pointer's overloaded subscription under ``MetaType``.

        Python gives a metaclass ``__getitem__`` priority over a class's
        ``__class_getitem__``.  Custom cstruct types need the former for array
        construction, while pointers also use the latter for ``Pointer[T]``
        annotations.  This opt-in hook keeps that distinction local to the one
        type that needs it.
        """
        return cls.__class_getitem__(item)

    def dereference(self, *, endian: str | None = None) -> Any:
        """Read the addressed value out of the stream this pointer came from.

        A null pointer raises rather than reading offset zero: on a forensic
        image offset zero is real data, and it would decode into something
        plausible and wrong.

        A successful read restores the stream position, because following a
        pointer must not move the cursor the caller is using. Current upstream
        does not restore after a failed target read; the source remains where
        that reader failed, and this implementation preserves that observable
        behavior.
        """
        if int(self) == 0:
            raise NullPointerDereference("cannot dereference a null pointer")

        target = type(self)._target()
        if target is None or self.__stream is None:
            raise NullPointerDereference(
                "this pointer carries no target type or stream, so it cannot be dereferenced"
            )

        order = endian if endian is not None else self.__endian
        position = self.__stream.tell()
        self.__stream.seek(int(self))
        if _reads_bytes(target):
            # A `char *` addresses a null-terminated string, not one byte.
            # Upstream makes the same assumption, in the same place.
            target = target[None]
        if order is not None:
            try:
                value = target(self.__stream, endian=order)
            except TypeError:
                # Not every target type takes an override; dereferencing in
                # the namespace's own byte order beats refusing outright.
                self.__stream.seek(int(self))
            else:
                self.__stream.seek(position)
                return value
        value = target(self.__stream)
        self.__stream.seek(position)
        return value

    def __getattr__(self, attr: str) -> Any:
        """Reading an attribute off a pointer reads it off what it addresses.

        `obj.ptr.magic` is how a pointer to a structure is used, and it is the
        reason this class is worth having at all. Names beginning with an
        underscore are excluded so the instance's own bookkeeping — and any
        dunder lookup Python performs — does not trigger a stream read.
        """
        if attr.startswith("_"):
            raise AttributeError(attr)
        return getattr(self.dereference(), attr)

    def __repr__(self) -> str:
        """`<uint32* @ 0x4>` -- the *resolved* target's name, then the address.

        Resolved rather than taken from the class name, because a declaration
        writes whatever alias it likes: `uint8_t *p` and `uint8 *p` are the same
        pointer and upstream reprs both as `uint8*`. Taking the class name
        printed the alias, which is a difference visible in any log that dumps a
        record.
        """
        target = type(self)._target()
        name = (
            getattr(target, "__name__", None)
            or getattr(target, "name", None)
            or type(self).__name__.removesuffix("*")
        )
        return f"<{name}* @ 0x{int(self):x}>"

    def __str__(self) -> str:
        """The addressed *value*, which is what upstream prints.

        `str(ptr)` on a `uint8*` at address 1 gives `"255"`, not `"1"`. That
        reads oddly out of context and it is deliberate: a pointer's interesting
        content is what it points at.
        """
        return str(self.dereference())


def _rebuild(name: str) -> Callable[..., Any]:
    """Wrap one `int` operator so it returns a pointer, not a bare int.

    `int.__add__` returns a plain `int`, so `ptr += 1` would quietly stop being
    a pointer and the next `.dereference()` would fail with an `AttributeError`
    about `int`. Every operator that yields an address is rebuilt as the same
    pointer type over the same stream.
    """
    operation = getattr(int, name)

    def wrapper(self: Pointer, other: Any) -> Any:
        result = operation(self, other)
        if result is NotImplemented or not isinstance(result, int):
            return result
        return type(self)(
            int(result),
            self._Pointer__stream,
            context=self._Pointer__context,
            endian=self._Pointer__endian,
        )

    wrapper.__name__ = name
    wrapper.__qualname__ = f"Pointer.{name}"
    wrapper.__doc__ = (
        f"`{name}` on the address, keeping the stream so the result is still a pointer."
    )
    return wrapper


for _name in (
    "__add__",
    "__radd__",
    "__sub__",
    "__mul__",
    "__floordiv__",
    "__mod__",
    "__pow__",
    "__lshift__",
    "__rshift__",
    "__and__",
    "__xor__",
    "__or__",
):
    setattr(Pointer, _name, _rebuild(_name))
del _name


__all__ = ["Pointer", "PointerArray"]
