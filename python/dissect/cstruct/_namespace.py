"""Definition-time helpers bound onto the ``cstruct`` namespace class.

# What this module does when it is imported

It mutates the Rust ``cstruct`` class, adding namespace methods and properties
that do not belong on the per-record native path:
``cdef``, ``loadfile``, ``resolve``, ``types``, ``void``, ``_make_enum``,
``_make_flag``, ``_make_int_type``, ``_make_union``, ``_make_struct`` and
``_make_pointer``.

Importing this module is therefore not side-effect-free, and the package root
imports it for exactly that side effect. The alternative — writing eleven trivial
methods in Rust — would cost a rebuild to change any of them and buy nothing:
every one is reached when a definition is loaded, never per record.

Current upstream uses the private ``_make_*`` factories internally when it
binds types to a namespace. Providing the same callable surface also lets code
construct types without spelling an equivalent definition by hand.

# Most of them go through ``load``, and one deliberately does not

``_make_int_type``, ``_make_union`` and ``_make_struct`` all build a definition
string and hand it to ``load``, rather than constructing a type object directly.
The schema owns type identity: a type built behind its back would not be reachable
as ``cs.<name>`` nor usable in a later definition.

``_make_pointer`` is the exception, because a pointer type is not a schema entry.
The schema knows that a pointer *member* reads a pointer-sized integer, which is
all decoding needs; what a ``Pointer`` subclass adds is the target type and the
stream, and neither is expressible in a definition.
"""

from __future__ import annotations

import inspect
import struct
import types
import warnings
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

from dissect.cstruct._cstruct_rs2 import cstruct
from dissect.cstruct._enum_types import _make_enum_class
from dissect.cstruct.exception import ResolveError


class _VoidArrayValue(list[Any]):
    """A zero-width array value; its declared count never consumes bytes."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__()

    def dumps(self) -> bytes:
        return b""

    @classmethod
    def __default__(cls) -> list[Any]:
        return []


_VOID_ARRAY_TYPES: dict[Any, type[_VoidArrayValue]] = {}


class _VoidMeta(type):
    """Create and cache the array type used by ``void[count]``."""

    def __getitem__(cls, count: Any) -> type[_VoidArrayValue]:
        if count not in _VOID_ARRAY_TYPES:
            suffix = "" if count is None else str(count)
            _VOID_ARRAY_TYPES[count] = type(
                f"void[{suffix}]",
                (_VoidArrayValue,),
                {
                    "__size__": 0,
                    "__dynamic__": False,
                    "__alignment__": 1,
                    "__type__": cls,
                    "__num_entries__": count,
                    "__null_terminated__": count is None,
                },
            )
        return _VOID_ARRAY_TYPES[count]


class Void(metaclass=_VoidMeta):
    """A zero-width type: reads nothing, writes nothing, and is falsy.

    Upstream uses ``void`` as a placeholder in unions and as an explicitly-empty
    member. The type object is truthy (it is a class) while an *instance* is
    falsy, which is what lets ``if record.field:`` distinguish "absent" from
    "present but zero".

    Reading consumes no input: ``cs.void(stream)`` leaves the stream position
    untouched, so a ``void`` member in a union does not disturb the cursor.
    """

    __slots__ = ()
    size = 0
    dynamic = False
    __size__ = 0
    __dynamic__ = False
    __alignment__ = 1

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        # Accepts and ignores a buffer or stream: reading nothing is the point,
        # and refusing the argument would make `void` unusable as a field type.
        pass

    def __bool__(self) -> bool:
        return False

    def __len__(self) -> int:
        return 0

    def __repr__(self) -> str:
        return "<void>"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Void)

    def __hash__(self) -> int:
        return hash(Void)

    def dumps(self) -> bytes:
        """A void encodes to no bytes at all."""
        return b""

    @classmethod
    def __default__(cls) -> Void:
        return cls()


# Bound as a class-level property rather than registered in the Rust schema:
# `void` has no width, no byte order and no decode op, so a `Scalar` variant for
# it would thread a special case through the plan compiler and the decoder for a
# type that reads nothing. Python attribute lookup finds this before the Rust
# `__getattr__` fallback runs, so `cs.void` resolves without touching the
# registry.
cstruct.void = property(lambda self: Void)


def _make_type(
    self: Any,
    name: str,
    bases: Any,
    size: int | None,
    *,
    alignment: int | None = None,
    attrs: dict[str, Any] | None = None,
) -> type:
    """Create a Python type bound to this namespace.

    Native built-ins do not use this factory.  It exists for upstream's custom
    type protocol, where a plugin supplies a Python base class and cstruct adds
    stable size, alignment, and namespace metadata once at registration.
    """
    namespace = dict(attrs or {})
    namespace.update(
        {
            "__cs__": self,
            "__size__": size,
            "__dynamic__": size is None,
            "__alignment__": alignment if alignment is not None else size,
        }
    )
    return types.new_class(name, tuple(bases), {}, lambda target: target.update(namespace))


def _make_array(self: Any, type_: type, count: Any) -> type:
    """Create the array class selected by a consumer-defined type."""
    null_terminated = count is None
    element_size = getattr(type_, "__size__", None)
    size = count * element_size if isinstance(count, int) and element_size is not None else None
    suffix = "" if null_terminated else str(count)
    return self._make_type(
        f"{type_.__name__}[{suffix}]",
        (type_.__ArrayType__,),
        size,
        alignment=getattr(type_, "__alignment__", None),
        attrs={
            "__type__": type_,
            "__num_entries__": count,
            "__null_terminated__": null_terminated,
        },
    )


cstruct._make_type = _make_type
cstruct._make_array = _make_array


def _make_native_array_alias(
    self: Any,
    name: str,
    native_type: Any,
    element_type: Any,
    count: Any,
) -> type:
    """Publish a native built-in array typedef as an upstream-style class.

    The class exists only for direct namespace access such as ``cs.uuid_t``.
    Fields declared with the alias remain compiled native array operations.
    """
    from dissect.cstruct._bases import NativeArrayAlias

    size = getattr(native_type, "__size__", None)
    return self._make_type(
        name,
        (NativeArrayAlias,),
        size,
        alignment=getattr(element_type, "__alignment__", None),
        attrs={
            "__native_type__": native_type,
            "__type__": element_type,
            "__num_entries__": count,
            "__null_terminated__": count is None,
        },
    )


cstruct._make_native_array_alias = _make_native_array_alias


def _scalar_shape(type_: Any) -> tuple[int, bool]:
    """Validate an enum base and return its encoded size and signedness."""
    scalar_integer = bool(getattr(type_, "_is_integer", False))
    class_integer = isinstance(type_, type) and issubclass(type_, int)
    if not scalar_integer and not class_integer:
        raise TypeError("Enum can only be created from int type")

    size = getattr(type_, "__size__", None) or 4
    explicit_signed = getattr(type_, "signed", None)
    if explicit_signed is not None:
        return size, bool(explicit_signed)
    name = str(getattr(type_, "name", "") or getattr(type_, "__name__", ""))
    # `int8` is signed, `uint8` is not. The leading `u` is the whole rule, and
    # it is upstream's rule too.
    return size, not name.startswith(("u", "U"))


def _make_enum(self: Any, name: str, type_: Any, members: dict[str, int]) -> type:
    """Build an enum class over `type_` without going through a definition."""
    size, signed = _scalar_shape(type_)
    return _make_enum_class(
        name,
        dict(members),
        False,
        size,
        self.endian != ">",
        signed,
        owner=self,
        base_type=type_,
    )


def _make_flag(self: Any, name: str, type_: Any, members: dict[str, int]) -> type:
    """Build a flag class over `type_` without going through a definition."""
    size, signed = _scalar_shape(type_)
    return _make_enum_class(
        name,
        dict(members),
        True,
        size,
        self.endian != ">",
        signed,
        owner=self,
        base_type=type_,
    )


def _make_int_type(
    self: Any, name: str, size: int, signed: bool, *, alignment: int | None = None
) -> Any:
    """Define an integer type of arbitrary width and return it.

    Registered with the native schema rather than constructing a detached type
    object: the schema owns type identity, and a type built behind its back
    would not be reachable as ``cs.<name>`` nor usable in a later definition.
    """
    self._register_int_type(name, size, signed)
    type_object = getattr(self, name)
    # Upstream's factory uses its arbitrary-width ``Int`` implementation even
    # for a 1-, 2-, 4-, or 8-byte request. Valid bytes match the packed builtins;
    # out-of-range values must raise ``OverflowError`` rather than
    # ``struct.error``.
    type_object._use_arbitrary_integer_writer()
    if alignment is not None:
        type_object._set_alignment(alignment)
    return type_object


def _make_packed_type(
    self: Any,
    name: str,
    fmt: str,
    base: type,
    *,
    alignment: int | None = None,
) -> type:
    """Create the small Python packed type exposed by upstream's factory."""
    size = struct.calcsize(fmt)

    def _read(
        cls: type,
        stream: Any,
        *,
        context: dict[str, Any] | None = None,
        endian: str,
    ) -> Any:
        del context
        raw = stream.read(size)
        if len(raw) != size:
            raise EOFError("unexpected end of stream")
        effective = fmt if fmt[:1] in "@=<>!" else endian + fmt
        return cls(struct.unpack(effective, raw)[0])

    def _write(cls: type, stream: Any, data: Any, *, endian: str) -> int:
        effective = fmt if fmt[:1] in "@=<>!" else endian + fmt
        raw = struct.pack(effective, data)
        return stream.write(raw)

    from dissect.cstruct._bases import BaseType

    return self._make_type(
        name,
        (base, BaseType),
        size,
        alignment=alignment,
        attrs={"__fmt__": fmt, "_read": classmethod(_read), "_write": classmethod(_write)},
    )


cstruct._make_enum = _make_enum
cstruct._make_flag = _make_flag
cstruct._make_int_type = _make_int_type
cstruct._make_packed_type = _make_packed_type


_native_add_custom_type = cstruct.add_custom_type


def _method_descriptor(type_: type, name: str) -> Any | None:
    """Find the descriptor that supplies ``name`` to ``type_``.

    Reading the attribute normally returns a class method already bound to the
    unregistered source class.  Registration creates a namespace-bound
    subclass, so an adapter must retain the descriptor's function and bind it
    to that new subclass instead.
    """
    for base in type_.__mro__:
        if name in base.__dict__:
            return base.__dict__[name]
    return None


def _legacy_method_adapter(type_: type, method_name: str) -> Any | None:
    """Adapt a pre-5.0 custom callback to the current keyword protocol.

    dissect.cstruct 5.0 added ``context`` and ``endian`` keyword arguments to
    custom readers and writers.  Older Dissect plugins remain useful, and the
    4.7 API allowed callbacks that accept neither keyword.  Inspecting once at
    registration keeps those callbacks working without signature checks or
    retry logic on each custom-field read.
    """
    method = getattr(type_, method_name, None)
    if method is None:
        return None

    signature = inspect.signature(method)
    parameters = signature.parameters
    if any(parameter.kind is inspect.Parameter.VAR_KEYWORD for parameter in parameters.values()):
        return None

    accepted_keywords = {
        name
        for name, parameter in parameters.items()
        if parameter.kind
        in (inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY)
    }
    supplied_keywords = {"endian"}
    if method_name.startswith("_read"):
        supplied_keywords.add("context")
    if supplied_keywords <= accepted_keywords:
        return None

    descriptor = _method_descriptor(type_, method_name)
    if isinstance(descriptor, classmethod):
        function = descriptor.__func__

        def adapted(cls: type, *args: Any, **kwargs: Any) -> Any:
            compatible = {
                name: value for name, value in kwargs.items() if name in accepted_keywords
            }
            return function(cls, *args, **compatible)

        adapted.__name__ = function.__name__
        adapted.__doc__ = function.__doc__
        return classmethod(adapted)

    if isinstance(descriptor, staticmethod):
        function = descriptor.__func__

        def adapted(*args: Any, **kwargs: Any) -> Any:
            compatible = {
                name: value for name, value in kwargs.items() if name in accepted_keywords
            }
            return function(*args, **compatible)

        adapted.__name__ = function.__name__
        adapted.__doc__ = function.__doc__
        return staticmethod(adapted)

    # A custom callback is normally a class method.  Leave unusual descriptors
    # untouched rather than guessing how their binding is meant to work.
    return None


def _legacy_method_adapters(type_: type) -> dict[str, Any]:
    """Return cold-path adapters needed by one consumer-supplied type."""
    result: dict[str, Any] = {}
    for method_name in ("_read", "_read_array", "_read_0", "_write", "_write_array", "_write_0"):
        if (adapter := _legacy_method_adapter(type_, method_name)) is not None:
            result[method_name] = adapter
    return result


def _add_custom_type(
    self: Any,
    name: str,
    type_: type,
    size: int | None = None,
    alignment: int | None = None,
    **kwargs: Any,
) -> None:
    """Bind and register a consumer type, warning about legacy callbacks.

    Current upstream passes byte order as a required keyword-only argument.
    Detecting an older callback here keeps the record decoder simple and gives
    plugin maintainers a useful warning before the callback is exercised.
    """
    from dissect.cstruct._bases import BaseType

    array_type = getattr(type_, "__ArrayType__", BaseType.__ArrayType__)
    for candidate in (type_, array_type):
        candidate_name = type_.__name__
        if candidate is array_type:
            candidate_name += f".{candidate.__name__}"
        methods = ("_read", "_read_array", "_read_0", "_write", "_write_array", "_write_0")
        for method_name in methods:
            method = getattr(candidate, method_name, None)
            if method is None or "endian" in inspect.signature(method).parameters:
                continue
            warnings.warn(
                f"Custom type {candidate_name} is missing the 'endian' keyword-only parameter "
                f"in its {method_name} method. Please refer to the changelog of "
                "dissect.cstruct 5.0 for more information.",
                UserWarning,
                stacklevel=2,
            )

    array_adapters = _legacy_method_adapters(array_type)
    if array_adapters:
        array_type = types.new_class(
            f"_{array_type.__name__}LegacyAdapter",
            (array_type,),
            {},
            lambda namespace: namespace.update(array_adapters),
        )

    bound_attrs = dict(kwargs)
    bound_attrs.update(_legacy_method_adapters(type_))
    bound_attrs["__ArrayType__"] = array_type

    bases = (type_,) if issubclass(type_, BaseType) else (type_, BaseType)
    bound = self._make_type(
        name,
        bases,
        size,
        alignment=alignment,
        attrs=bound_attrs,
    )
    _native_add_custom_type(self, name, bound, size, alignment, **kwargs)


cstruct.add_custom_type = _add_custom_type
cstruct.addcustomtype = _add_custom_type


def _cdef(self: Any) -> str:
    """Render this namespace as semantically equivalent cstruct source.

    Upstream walks its Python type graph. This implementation walks the schema
    retained by the Rust registry, so rendering does not parse the original
    text again and cannot put work on record decoding.
    """
    return self._render_cdef()


def _loadfile(self: Any, path: Any, *args: Any, **kwargs: Any) -> Any:
    """Load a definition from a file path or open file object."""
    if hasattr(path, "read"):
        self.load(path.read(), *args, **kwargs)
        return None
    with open(path) as handle:
        self.load(handle.read(), *args, **kwargs)
    return None


def _resolve(self: Any, name: Any) -> Any:
    """Resolve a type name to its type object, following typedefs."""
    if not isinstance(name, str):
        return name
    try:
        return self._resolve_type(name)
    except AttributeError:
        raise ResolveError(f"Unknown type {name}") from None


def _read(self: Any, name: str, source: Any) -> Any:
    """Decode one value using a type selected by definition-language name."""
    type_object = self.resolve(name)
    if native := getattr(type_object, "_cstruct_type_", None):
        return native.read(source)
    return type_object.read(source)


@contextmanager
def _endianness(self: Any, endian: str) -> Iterator[Any]:
    """Temporarily change byte order, restoring it even after an exception."""
    original = self.endian
    self.endian = endian
    try:
        yield self
    finally:
        self.endian = original


def _big_endian(self: Any) -> Any:
    """Return a context that temporarily selects big-endian decoding."""
    return self.endianness(">")


def _little_endian(self: Any) -> Any:
    """Return a context that temporarily selects little-endian decoding."""
    return self.endianness("<")


cstruct.cdef = _cdef
cstruct.loadfile = _loadfile
cstruct.resolve = _resolve
cstruct.read = _read
cstruct.endianness = _endianness
cstruct.big_endian = _big_endian
cstruct.little_endian = _little_endian
cstruct.__copy__ = lambda self: self.copy()
# `types` is current upstream's canonical mapping name; its deprecated
# `typedefs` property addresses the same dictionary. The native registry exposes
# `typedefs`, so bind `types` to that same live mapping rather than copying it.
cstruct.types = property(lambda self: self.typedefs)


def _type_name(type_object: Any) -> str:
    """The definition-language name of a type object."""
    return str(getattr(type_object, "name", None) or getattr(type_object, "__name__", ""))


def _make_union(self: Any, name: str, fields: Any, **kwargs: Any) -> type:
    """Build a union type from `Field` objects rather than a definition string.

    Routed through `load` so the schema owns the type: a type built behind its
    back would not be reachable as `cs.<name>` nor usable in a later
    definition, which is the same reasoning as `_make_int_type`.
    """
    body = "".join(
        f"    {_type_name(field.type)} {field.name}"
        + (f" : {field.bits}" if getattr(field, "bits", None) else "")
        + ";\n"
        for field in fields
    )
    self.load(f"union {name} {{\n{body}}};", **kwargs)
    return getattr(self, name)


def _make_struct(self: Any, name: str, fields: Any, **kwargs: Any) -> type:
    """Build a struct type from `Field` objects. See :func:`_make_union`."""
    body = "".join(
        f"    {_type_name(field.type)} {field.name}"
        + (f" : {field.bits}" if getattr(field, "bits", None) else "")
        + ";\n"
        for field in fields
    )
    self.load(f"struct {name} {{\n{body}}};", **kwargs)
    return getattr(self, name)


cstruct._make_union = _make_union
cstruct._make_struct = _make_struct


def _make_pointer(self: Any, target: Any) -> type:
    """Build a pointer type addressing `target`.

    Not routed through `load`, unlike `_make_union` and `_make_int_type`: a
    pointer type is not a schema entry. The schema knows a pointer *member*
    reads a pointer-sized integer, which is all decoding needs; what a
    `Pointer` subclass adds is the target type and the stream, and neither of
    those is expressible in a definition.

    The width is *not* captured here: `Pointer` reads it from `__cs__.pointer`
    on every read and write, so `cs.pointer = cs.uint16` changes existing
    pointer types too. A structure's immutable plan captures its width and byte
    order for one compiled generation; changing namespace settings replaces
    that generation while keeping the visible class stable.
    """
    from dissect.cstruct.types.pointer import Pointer, _LazyPointerTarget

    # A *name* rather than a type object is the decoder's spelling: it builds
    # pointer classes while compiling the enclosing type, and resolving the
    # target then would ask for a type that is still being built.
    # `struct Node { Node *next; }` is the ordinary case for that.
    if isinstance(target, str):
        canonical = self._canonical_type_name(target)
        cached = self._pointer_type(canonical)
        if cached is not None:
            return cached
        if canonical.endswith("*"):
            # `uint32 **p` addresses a `uint32*`, which is itself a pointer.
            inner = _make_pointer(self, canonical[:-1])
            built = type(f"{canonical}*", (Pointer,), {"__type__": inner, "__cs__": self})
        else:
            built = type(
                f"{canonical}*",
                (Pointer,),
                {
                    "__type__": _LazyPointerTarget(canonical),
                    "__type_name__": canonical,
                    "__cs__": self,
                },
            )
        self._cache_pointer_type(canonical, built)
        return built

    name = getattr(target, "__name__", None) or getattr(target, "name", None) or "?"
    canonical = self._canonical_type_name(name)
    # Current upstream builds a fresh class for every direct factory call. Plan
    # runtimes use the textual branch above and cache one class per canonical
    # target so a materialised field retains its identity across an endian
    # recompilation; a caller supplying a type object gets the source factory's
    # fresh-class behaviour.
    return type(f"{canonical}*", (Pointer,), {"__type__": target, "__cs__": self})


cstruct._make_pointer = _make_pointer


__all__ = ["Void"]
