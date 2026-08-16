"""The Python class that stands in front of a compiled structure type.

``cs.MY_STRUCT`` is a class built here, once per type, wrapping the Rust
``StructType`` that does the actual decoding.

# Why it has to be a class at all

Callers write ``isinstance(value, cs.MY_STRUCT)``, and ``isinstance`` requires its
second argument to be a type before any hook it might define is consulted. So a
factory object will not do, however much simpler it would be.

# Why decoding still costs no Python frame

The decode entry point is ``__new__``, and ``__new__`` is the Rust function
``struct_new``. ``cls(stream)`` goes ``type.__call__`` → ``cls.__new__`` entirely
in C and lands in Rust. The record that comes back is deliberately *not* an
instance of ``cls``, so CPython skips ``__init__`` as well.

That is also why ``__instancecheck__`` exists here: since records are not
instances, something has to make ``isinstance`` tell the truth. It compares a
namespace-owned logical type identity rather than names, so two namespaces may
declare ``struct Entry`` without sharing a type. The identity remains stable
when that namespace recompiles the type for a new byte order.

# Metadata stays off the record path

Canonical metadata is retained once by the compiled type. Deprecated upstream
spellings are metaclass properties because they must emit warnings, but none of
these lookups runs while a record is decoded.
"""

from __future__ import annotations

import io
import warnings
from collections.abc import MutableMapping

from dissect.cstruct._cstruct_rs2 import Record, StructType, struct_new
from dissect.cstruct._streams import _as_stream
from dissect.cstruct.exception import ArraySizeError


class _StructureMarkerMeta(type):
    """Connect generated type classes and native decoded records.

    Upstream uses one Python inheritance tree for both. rs2 deliberately keeps
    decoded records native and generated type classes lightweight, so the
    marker performs the instance side of that relationship without adding a
    Python base or wrapper to each decoded value.
    """

    def __instancecheck__(cls, obj: object) -> bool:
        if not isinstance(obj, Record):
            return False
        if cls is Union:
            return obj._is_union
        return True


class Structure(metaclass=_StructureMarkerMeta):
    """Base marker for generated structures and native structure records.

    Generated classes inherit this marker, while decoded values remain native
    ``Record`` objects. ``isinstance(record, Structure)`` therefore matches
    upstream without placing another Python object on the decode path.
    """


class Union(Structure):
    """Base class for union types.

    Unions decode correctly regardless — members overlay at offset zero, handled
    in Rust — but a generated type built with empty bases would be
    indistinguishable from a struct at the Python level, so ``issubclass(t, Union)``
    could not be true. The compiled plan knows which it is; this class is how that
    fact reaches Python.

    Generated union classes inherit this marker. Native records are recognised
    only when their compiled plan is a union, so a normal structure does not
    become a union merely because both use the same decoder.
    """


class UnionProxy:
    """Upstream's write-through view over a union member. Not implemented.

    Named so ``from dissect.cstruct.types.structure import UnionProxy`` resolves.
    Decoding a union here returns an ordinary record rather than a proxy, so this
    is deliberately not a working stand-in — a class that looked like one would be
    worse than a name that clearly is not.
    """


class _RecordValues(MutableMapping[str, object]):
    """Write-through view of a native record's public member values.

    Upstream exposes ``StructureValuesProxy`` from ``types.structure``. Its
    mapping includes fields folded out of anonymous structures, and assignment
    changes the record rather than a copied dictionary. The native record gives
    this proxy the ordered public names because it cannot inherit the generated
    type's ``__members__`` mapping.
    """

    def __init__(self, record: Record):
        self._record = record

    def __getitem__(self, key: str) -> object:
        if key in self:
            return self._record[key]
        raise KeyError(key)

    def __setitem__(self, key: str, value: object) -> None:
        if key in self:
            self._record[key] = value
            return
        raise KeyError(key)

    def __delitem__(self, key: str) -> None:
        raise NotImplementedError("Cannot delete fields from a Structure")

    def __contains__(self, key: object) -> bool:
        return key in self._record._member_names

    def __iter__(self):
        return iter(self._record._member_names)

    def __len__(self) -> int:
        return len(self._record._member_names)

    def __repr__(self) -> str:
        return repr(dict(self))


class StructureMetaType(_StructureMarkerMeta):
    """Metaclass for generated struct classes.

    Carries the operations that belong on the *class* rather than on a record:
    ``len(cls)``, and the instance check that makes ``isinstance`` work.
    """

    def __len__(cls) -> int:
        """Size in bytes. `dissect.extfs` sizes group descriptors with this."""
        size = cls.__size__
        if size is None:
            raise TypeError(
                f"Dynamic size: {cls.__name__} has no fixed size, so len() is undefined"
            )
        return size

    def __default__(cls):
        """Return a new record containing each member type's default value."""
        return cls()

    def cdef(cls, *, recursive: bool = False) -> str:
        """Render this type as a C-style declaration.

        ``recursive=True`` also emits referenced structures, enums and array
        constants, making the result suitable for loading into a fresh cstruct
        namespace. As in upstream, the result is equivalent source rather than
        a byte-for-byte copy of the original formatting.
        """
        return cls.__cs__._render_type_cdef(cls._cstruct_type_.__name__, recursive=recursive)

    def add_field(
        cls,
        name: str,
        type_: object,
        bits: int | None = None,
        offset: int | None = None,
    ) -> None:
        """Append a field and publish a new compiled type generation.

        Existing records keep the plan that decoded them. The generated class
        remains the same object, while future reads use the new layout. Calls
        inside :meth:`start_update` are compiled together when the context
        closes.
        """
        type_name = str(getattr(type_, "__name__", None) or getattr(type_, "name", ""))
        entry = (name, type_name, bits, offset)
        pending = cls.__dict__.get("_pending_field_updates_")
        if pending is not None:
            pending.append(entry)
            return
        cls.__cs__._add_struct_fields(cls._cstruct_type_, [entry])
        _refresh_struct_metadata(cls)

    def start_update(cls):
        """Batch several :meth:`add_field` calls into one plan rebuild."""
        return _StructureUpdate(cls)

    @property
    def __cs__(cls):
        """Namespace that owns this generated structure type.

        Upstream stores the namespace on each generated type. The compiled
        type already retains the same owner, so reading it here avoids adding
        another strong edge to the namespace/type cycle.
        """
        return cls._cstruct_type_.__cs__

    @property
    def __size__(cls):
        """Fixed byte size retained by the immutable compiled type."""
        return cls._cstruct_type_.size

    @property
    def __dynamic__(cls):
        """Whether field contents determine this type's byte size."""
        return cls._cstruct_type_.dynamic

    @property
    def cs(cls):
        """Deprecated spelling for :attr:`__cs__`, retained for upstream callers."""
        warnings.warn(
            "The 'cs' property is deprecated, use '__cs__' instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__cs__

    @property
    def size(cls):
        """Deprecated spelling for the fixed byte size."""
        warnings.warn(
            "The 'size' property is deprecated, use '__size__', len(cls) or sizeof(cls) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__size__

    @property
    def dynamic(cls):
        """Deprecated spelling for :attr:`__dynamic__`."""
        warnings.warn(
            "The 'dynamic' property is deprecated, use '__dynamic__' instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__dynamic__

    @property
    def alignment(cls):
        """Deprecated spelling for the type's natural byte alignment."""
        warnings.warn(
            "The 'alignment' property is deprecated, use '__alignment__' instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__alignment__

    @property
    def fields(cls):
        """Deprecated spelling for the public member mapping."""
        warnings.warn(
            "The 'fields' property is deprecated, use '__members__' instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__members__

    @property
    def lookup(cls):
        """Deprecated spelling for the declared member mapping."""
        warnings.warn(
            "The 'lookup' property is deprecated, use '__lookup__' instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return cls.__lookup__

    def __getitem__(cls, count: int | None):
        """`cs.STRUCT[n]` -- an array of records; `[None]` is null-terminated.

        The null-terminated form reads records until one is entirely zero, which
        is how descriptor lists that end in a blank entry are declared.
        """
        return _StructArray(cls, count)

    def __instancecheck__(cls, obj: object) -> bool:
        """Recognise records this class produced.

        Compares logical type identity rather than type name: two namespaces
        may declare the same name and must remain distinct. Records are not
        subclass instances -- see ``struct_new`` in the Rust binding -- so this
        hook provides the normal upstream-facing ``isinstance`` behavior.
        """
        if not isinstance(obj, Record):
            return False
        return obj._plan_id == cls._cstruct_type_._plan_id


class _StructArray:
    """The result of subscripting a struct type.

    Upstream's array protocol pads a short fixed array with default records and
    appends one default record to a terminated array. This wrapper keeps that
    cold writer behavior in Python while every record still goes through the
    native checked encoder.
    """

    def __init__(self, struct_type: type, count: int | None):
        self._type = struct_type
        self._count = count

    @property
    def __name__(self) -> str:
        """Declared array spelling, matching upstream's type metadata."""
        suffix = "" if self._count is None else str(self._count)
        return f"{self._type.__name__}[{suffix}]"

    @property
    def __type__(self) -> type:
        """Structure type stored in each array element."""
        return self._type

    @property
    def __num_entries__(self) -> int | None:
        """Fixed element count, or ``None`` for a terminated array."""
        return self._count

    @property
    def __null_terminated__(self) -> bool:
        return self._count is None

    @property
    def __size__(self) -> int | None:
        element_size = self._type.__size__
        return None if self._count is None or element_size is None else self._count * element_size

    @property
    def __dynamic__(self) -> bool:
        return self.__size__ is None

    def __call__(self, source):
        stream = _as_stream(source)
        if self._count is not None:
            return [self._type(stream) for _ in range(self._count)]

        # Null-terminated: stop at the first all-zero record, and consume it.
        # That is upstream's behaviour and the terminator's bytes are part of
        # what the list occupied.
        size = self._type.__size__
        if size is None:
            raise TypeError(
                f"{self._type.__name__} is dynamically sized, so a "
                "null-terminated array of it has no terminator to look for"
            )
        out = []
        while True:
            raw = stream.read(size)
            if len(raw) != size:
                raise EOFError("unexpected end of stream")
            if not any(raw):
                return out
            out.append(self._type(io.BytesIO(raw)))

    def __default__(self):
        """Return the all-zero value for this declared array shape."""
        count = self._count or 0
        return [self._type() for _ in range(count)]

    def _write(self, stream, values, *, endian: str):
        """Write records using upstream's padding and terminator rules."""
        entries = list(values)
        if self._count is not None and len(entries) > self._count:
            raise ArraySizeError(
                f"Expected static array size {self._count}, got {len(entries)} instead."
            )

        if self._count is None:
            entries.append(self._type())
        else:
            entries.extend(self._type() for _ in range(self._count - len(entries)))

        return sum(self._type.write(stream, entry, endian=endian) for entry in entries)

    def write(self, stream, values, *, endian: str | None = None):
        """Write an array to a file-like object."""
        return self._write(
            stream,
            values,
            endian=self._type.__cs__.endian if endian is None else endian,
        )

    def dumps(self, values, *, endian: str | None = None) -> bytes:
        """Return the encoded array bytes."""
        stream = io.BytesIO()
        self.write(stream, values, endian=endian)
        return stream.getvalue()

    def __repr__(self) -> str:
        count = "None" if self._count is None else self._count
        return f"<{self._type.__name__}[{count}]>"


class _StructureUpdate:
    """Context manager used by ``StructureMetaType.start_update``."""

    def __init__(self, struct_type: type):
        self._type = struct_type

    def __enter__(self):
        if self._type.__dict__.get("_pending_field_updates_") is not None:
            raise RuntimeError("a structure update is already active")
        self._type._pending_field_updates_ = []
        return None

    def __exit__(self, exc_type, exc_value, traceback):
        pending = self._type._pending_field_updates_
        del self._type._pending_field_updates_
        if pending:
            self._type.__cs__._add_struct_fields(self._type._cstruct_type_, pending)
            _refresh_struct_metadata(self._type)
        return False


class _BoundConstructorInit:
    """No-op ``__init__`` bound only when a caller inspects a type.

    Structure creation is common while Dissect imports plugin definitions, but
    constructor introspection is not. Keeping the field owner here lets all
    generated classes share one descriptor and avoids allocating a helper and
    a second field-name tuple for every declared structure.
    """

    def __init__(self, owner: type):
        self._owner = owner

    def __call__(self, *args, **kwargs) -> None:
        return None

    @property
    def __signature__(self):
        from inspect import Parameter, Signature

        parameters = [Parameter("self", Parameter.POSITIONAL_OR_KEYWORD)]
        parameters.extend(
            Parameter(name, Parameter.POSITIONAL_OR_KEYWORD, default=None)
            for name in self._owner.__lookup__
        )
        return Signature(parameters)


class _ConstructorInitDescriptor:
    """Expose constructor signatures without per-structure setup work."""

    def __get__(self, instance: object, owner: type) -> _BoundConstructorInit:
        return _BoundConstructorInit(owner)


_CONSTRUCTOR_INIT = _ConstructorInitDescriptor()


def _refresh_struct_metadata(cls: type) -> None:
    """Refresh class-level views after its native generation changes."""
    struct_type = cls._cstruct_type_
    cls.__fast_path__ = struct_type.__fast_path__
    cls.__fields__ = struct_type.__fields__
    cls.__lookup__ = struct_type.lookup
    cls.__members__ = struct_type.fields
    cls.__static_sizes__ = struct_type.__static_sizes__
    cls.__align__ = struct_type.__align__
    cls.__alignment__ = struct_type.__alignment__


def _make_struct_class(name: str, struct_type: StructType) -> type:
    """Build the Python class for one struct definition.

    Called once per type by the Rust registry. Canonical attributes are stable
    for the lifetime of the type; deprecated aliases are supplied by the
    metaclass so their warnings remain outside record decoding.
    """
    members = struct_type.fields
    lookup = struct_type.lookup
    namespace = {
        "__new__": staticmethod(struct_new),
        # The native ``__new__`` returns a Record, so this is never executed.
        # It exists solely to expose upstream's constructor signature without
        # generating one Python function per declared structure.
        "__init__": _CONSTRUCTOR_INIT,
        "_cstruct_type_": struct_type,
        # `lookup` is the *declared* field mapping: anonymous members keep
        # their generated name rather than being folded into the parent, and
        # each entry carries its offset. Callers slice buffers by those offsets.
        "__fast_path__": struct_type.__fast_path__,
        # Upstream exposes encoding on the type as well as on the record, so
        # `cs.TYPE.dumps(record)` works. Bound here rather than reached through
        # the metaclass, so it stays a plain attribute lookup.
        "dumps": staticmethod(struct_type.dumps),
        "read": staticmethod(struct_type.read),
        "reads": staticmethod(struct_type.reads),
        # Current downstream writers use both spellings: Hypervisor writes a
        # freshly constructed header through the record instance, while a type
        # selected from a map is written through the class.  Both delegate to
        # the same native checked serializer.
        "write": staticmethod(struct_type.write),
        # Always False, and deliberately not a reflection of what
        # ``load(compiled=…)`` was asked for.
        #
        # Upstream's ``__compiled__`` means "this type's reader was generated as
        # Python bytecode by dissect.cstruct's own compiler". That never happens
        # here -- decoding goes through the compiled Rust plan instead -- so the
        # honest answer is always False. ``load(compiled=...)`` is accepted and
        # ignored for the same reason; echoing the flag back would report a
        # state that does not exist.
        "__compiled__": False,
        # Upstream exposes both views: `__fields__` is the ordered sequence and
        # `__lookup__` is the name-keyed mapping.
        "__fields__": struct_type.__fields__,
        "__lookup__": lookup,
        # Pointer members and what each addresses. Empty for nearly every type,
        # but required to resolve pointer targets where they occur.
        "__pointers__": struct_type.__pointers__,
        # Upstream keys the declared members by name on the type; `lookup` is
        # exactly that mapping (anonymous members keep their generated name and
        # each entry carries its offset).
        "__members__": members,
        "__static_sizes__": struct_type.__static_sizes__,
        # `__align__` says whether aligned layout was requested;
        # `__alignment__` is the resulting byte boundary. Both come from the
        # compiled plan rather than being recomputed here.
        "__align__": struct_type.__align__,
        "__alignment__": struct_type.__alignment__,
        "__doc__": f"cstruct type {name}",
        "__slots__": (),
    }
    # The generated class carries the same marker relationship upstream's
    # generated classes do. Records remain native; the marker metaclass bridges
    # that separate instance representation for ``isinstance``.
    bases = (Union,) if struct_type.__is_union__ else (Structure,)
    return StructureMetaType(name, bases, namespace)


__all__ = [
    "Structure",
    "StructureMetaType",
    "Union",
    "UnionProxy",
    "_make_struct_class",
]
