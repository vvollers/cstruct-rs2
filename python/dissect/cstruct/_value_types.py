"""Value objects: what a type object returns when it is *called with a value*.

# The seam this module implements, and why it exists

Upstream returns a type-carrying subclass instance from everything a type object
does: ``cs.uint32(5)`` is a ``uint32`` holding 5, and so is the ``a`` field of a
decoded record. Both know how to ``dumps()`` themselves.

Decoding here returns plain ``int``, ``bytes``, ``str`` and ``float`` instead,
because wrapping every field of every record in a Python subclass costs an
allocation per field — which is most of what this implementation exists to avoid.

*Constructing* a value is a different matter. It happens once per call, not once
per field per record, so it costs nothing to support. Hence the split:

* a type object called with a **value** returns a value object;
* a type object called with a **buffer or stream** returns the native type.

The seam is visible to callers and worth knowing about: ``cs.uint32(5)`` is a
``uint32``, while ``record.a`` is a plain ``int``. Upstream returns the same type
for both.

# The base class is derived, not tabulated

A value class's Python base comes from ``type(type_object.__default__())`` rather
than from a table mapping cstruct types to Python types. ``__default__`` is
exactly "what an all-zero field of this type produces", so the answer is always
in step with the Rust side by construction; a table would have to be kept in
step by hand.
"""

from __future__ import annotations

from collections import OrderedDict
from typing import Any

from dissect.cstruct._bases import BaseType
from dissect.cstruct._cstruct_rs2 import ArrayType

#: Value classes, keyed by the id of the type object they belong to.
#:
#: The entry holds the type object as well as the class, which is what keeps the
#: id valid -- an id is only unique while its object is alive. Namespace scalar
#: types are themselves cached, so this is bounded by the number of distinct
#: scalar types a program touches. Array types are deliberately excluded: like
#: upstream's ``MetaType.__getitem__``, each subscription creates a fresh type.
_VALUE_CLASSES: dict[int, tuple[Any, type]] = {}

# Binary XML binds ``wchar[None].read`` once and calls it for every string, so
# null-terminated array types need to reuse their generated value class.  The
# cache is identity-based, just like the scalar cache, but capped: expressions
# such as ``cs.wchar[None](data)`` may create a new type on every call and must
# not turn attacker-controlled input into an unbounded process-global registry.
# Bounded runtime-count arrays remain wholly uncached because registry parsing
# creates one per value list in a much hotter loop.
_DYNAMIC_ARRAY_VALUE_CLASS_LIMIT = 8
_DYNAMIC_ARRAY_VALUE_CLASSES: OrderedDict[int, tuple[Any, type]] = OrderedDict()

# Private compatibility constant. Native type objects normalize byte-order
# overrides themselves; this set is importable but does not select encoding.
_BIG_ENDIAN = frozenset({">", "!", "big", "be", "network"})


def _value_class(type_object: Any) -> type:
    """Build (or fetch) the class this type object's constructor returns."""
    # Current upstream creates a new array class for every ``Type[count]``
    # expression.  The returned value keeps that class (and therefore its type
    # object) alive for exactly as long as the value is live.  A process-global
    # cache would instead retain every runtime count ever parsed; registry value
    # lists construct these types repeatedly and would make memory grow with the
    # whole query.  Scalar type objects are namespace-cached and remain safe to
    # reuse here.
    array_type = isinstance(type_object, ArrayType)
    dynamic_array = array_type and type_object.__dynamic__
    if dynamic_array:
        cache_key = id(type_object)
        cached = _DYNAMIC_ARRAY_VALUE_CLASSES.get(cache_key)
        if cached is not None and cached[0] is type_object:
            _DYNAMIC_ARRAY_VALUE_CLASSES.move_to_end(cache_key)
            return cached[1]
    elif not array_type:
        cached = _VALUE_CLASSES.get(id(type_object))
        if cached is not None and cached[0] is type_object:
            return cached[1]

    # The Python base comes from what the type decodes to, rather than from a
    # table that would have to be kept in step with the Rust side: `__default__`
    # is exactly "what an all-zero field of this type produces".
    base = type(type_object.__default__())

    def dumps(self: Any, endian: str | None = None) -> bytes:
        return type_object.dumps(self, endian=endian)

    @classmethod  # type: ignore[misc]
    def default(cls: type) -> Any:
        return cls(type_object.__default__())

    def write(self: Any, stream: Any, endian: str | None = None) -> int:
        return stream.write(self.dumps(endian))

    name = getattr(type_object, "__name__", None) or repr(type_object).strip("<>").split()[-1]
    cls = type(
        name,
        (base, BaseType),
        {
            "__type__": type_object,
            "__size__": type_object.__size__,
            "dumps": dumps,
            "_write": write,
            "write": write,
            "__default__": default,
            "__doc__": f"a {name} value",
        },
    )
    if dynamic_array:
        _DYNAMIC_ARRAY_VALUE_CLASSES[cache_key] = (type_object, cls)
        _DYNAMIC_ARRAY_VALUE_CLASSES.move_to_end(cache_key)
        while len(_DYNAMIC_ARRAY_VALUE_CLASSES) > _DYNAMIC_ARRAY_VALUE_CLASS_LIMIT:
            _DYNAMIC_ARRAY_VALUE_CLASSES.popitem(last=False)
    elif not array_type:
        _VALUE_CLASSES[id(type_object)] = (type_object, cls)
    return cls


def _make_value(type_object: Any, value: Any) -> Any:
    """Wrap `value` in this type object's value class."""
    return _value_class(type_object)(value)


__all__ = ["_make_value", "_value_class"]
