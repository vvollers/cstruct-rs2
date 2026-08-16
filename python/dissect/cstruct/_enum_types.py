"""Enum and flag classes with cstruct's semantics.

A definition's ``enum E : uint8 { ... }`` becomes a real Python ``IntEnum``
subclass, and ``flag F : uint16 { ... }`` an ``IntFlag``. That matters for
correctness rather than for tidiness: ``dissect.ntfs`` reads
``attribute.header.type.value`` off an enum field, and a plain ``int`` has no
``.value``.

The classes are built here, in Python, and not in Rust — even though the Rust
side is what asks for them (see ``_make_enum_class``, which the compiled plan
calls once per enum). Everything that has to match upstream about them is
Python-level behaviour:

* **``__str__`` is ``ClassName.MemberName``.** Python's ``IntEnum`` inherits
  ``int.__str__``, so ``str(member)`` would be the bare number. Upstream overrides
  it, and that string reaches anything that formats or logs a record.
* **An unrecognised value stays a member.** A forensic image routinely contains a
  value no definition names. Upstream turns it into a pseudo-member — ``repr`` is
  ``<E: 9>`` and the type is still ``E``. Returning a plain ``int`` would lose the
  type, which a consumer comparing against the enum would notice.
* **An enum type is callable on a stream.** Upstream's enums *are* cstruct types:
  ``c_mlocate.DBE_TYPE(stream)`` reads the underlying integer and returns the
  member. Plain ``IntEnum`` subclasses cannot do that — the call tries to look up
  the stream object as a member and raises — and the ``locate`` plugins' read loop
  is written as ``while ENUM(fh) != ENUM.END``.

Each namespace builds one class per declared enum. Member lookup, equality, and
hashing occur for decoded fields, so hashes are cached when members are created
and equality performs only the source-compatible family check before delegating
to the native integer operation.
"""

from __future__ import annotations

import enum
import functools
import sys

from dissect.cstruct._streams import _as_stream, _is_readable, _take
from dissect.cstruct.exception import ArraySizeError


def _enum_dumps(enum_type, value, *, endian: str | None = None) -> bytes:
    """Encode a member or integer through an enum's declared integer type."""
    number = value.value if isinstance(value, enum.Enum) else value
    return enum_type._encode_value(number, endian=endian)


def _enum_write(enum_type, stream, value, *, endian: str | None = None) -> int:
    """Write one enum value and return the sink's reported byte count."""
    return stream.write(_enum_dumps(enum_type, value, endian=endian))


class _EnumOverload:
    """Bind one encoder on both enum classes and enum member instances.

    Current ``dissect.cstruct`` exposes one descriptor for both
    ``Kind.write(stream, value)`` and ``Kind.VALUE.write(stream)``.  A regular
    method cannot distinguish those shapes on an ``IntEnum`` subclass because
    class lookup finds the member method before a metaclass method.
    """

    def __init__(self, function):
        self.function = function

    def __get__(self, instance, owner):
        if instance is None:
            return functools.partial(self.function, owner)
        return functools.partial(self.function, owner, value=instance)


class _EnumStr:
    """Upstream's ``__str__`` for enum and flag members.

    Python's ``IntEnum``/``IntFlag`` inherit ``int.__str__``, so ``str(member)``
    would be the bare number. Upstream overrides both to ``ClassName.MemberName``,
    and that string reaches anything that formats or logs a record.
    """

    def __str__(self) -> str:
        base = f"{type(self).__name__}." if type(self).__name__ else ""
        name = self.name if self.name is not None else str(self.value)
        return f"{base}{name}"

    def __repr__(self) -> str:
        # A value with no declared name reprs as ``<E: 9>``, not ``<E.None: 9>``
        # which is what Python's default produces for a pseudo-member.
        if self.name is None:
            return f"<{type(self).__name__}: {self.value}>"
        prefix = f"{type(self).__name__}." if type(self).__name__ else ""
        return f"<{prefix}{self.name}: {self.value}>"

    def __hash__(self) -> int:
        """Return the source-compatible hash cached when the member was made."""
        return self._cstruct_hash

    dumps = _EnumOverload(_enum_dumps)
    write = _EnumOverload(_enum_write)


def _byteorder(spelling: str | None, default: str) -> str:
    """Resolve the byte-order spellings accepted by the source type API."""
    if spelling is None:
        return default
    if spelling in {">", "!", "big", "be", "network"}:
        return "big"
    if spelling in {"<", "little", "le"}:
        return "little"
    if spelling in {"=", "@"}:
        return sys.byteorder
    raise ValueError(f"invalid endianness: {spelling!r}")


class _CEnumMeta(enum.EnumMeta):
    """Lets an enum type decode a value, as upstream's enum types do.

    Upstream's enums are cstruct *types*: ``c_mlocate.DBE_TYPE(stream)`` reads
    the underlying integer and returns the member. rs2 built plain ``IntEnum``
    subclasses, so the same call tried to look up a stream object as a member
    and raised — which silently cost every record from the ``locate`` plugins
    (228,071 on one image alone) because the plugin's read loop is written as
    ``while ENUM(fh) != ENUM.END``.

    Subscripting works too: ``ENUM[4](stream)`` reads four of them.
    """

    @property
    def __cs__(cls):
        """Return the live namespace that created this enum class.

        Upstream stores the namespace directly on the class. The namespace also
        caches the class, so doing that here would create an ownership cycle for
        every declared enum and flag. A weak reference preserves the public
        identity while the namespace is live without making short-lived command
        processes collect an otherwise unnecessary cycle at shutdown.
        """
        owner_ref = cls.__cstruct_owner_ref__
        return owner_ref() if owner_ref is not None else None

    def __call__(cls, value=None, *args, **kwargs):
        if _is_readable(value):
            return cls._read_one(value)
        if value is None:
            value = 0
        return super().__call__(value, *args, **kwargs)

    def __getitem__(cls, key):
        # `ENUM["NAME"]` is the standard member lookup; `ENUM[n]` is an array
        # type, which is upstream's spelling and not Python's.
        if isinstance(key, int) or key is None:
            return _EnumArray(cls, key)
        return super().__getitem__(key)

    def __len__(cls):
        # cstruct types report encoded width; stdlib EnumMeta reports members.
        return cls.__size__

    def __contains__(cls, value):
        if isinstance(value, cls):
            return True
        return value in cls._value2member_map_

    def _read_one(cls, source):
        raw = _take(source, cls.__size__)
        return cls(int.from_bytes(raw, cls.__byteorder__, signed=cls.__signed__))

    def _encode_value(cls, value: int, *, endian: str | None = None) -> bytes:
        return int(value).to_bytes(
            cls.__size__,
            _byteorder(endian, cls.__byteorder__),
            signed=cls.__signed__,
        )

    def dumps(cls, value, *, endian: str | None = None) -> bytes:
        """Encode a member or integer through the declared enum base."""
        number = value.value if isinstance(value, enum.Enum) else value
        return cls._encode_value(number, endian=endian)

    def write(cls, stream, value, *, endian: str | None = None) -> int:
        return stream.write(cls.dumps(value, endian=endian))

    def __default__(cls):
        return cls(0)

    def cdef(cls) -> str:
        """Render the enum or flag from its immutable class metadata."""
        keyword = "flag" if issubclass(cls, enum.IntFlag) else "enum"
        underlying = cls.__type__.__name__
        title = (
            f"{keyword} {cls.__name__} : {underlying}"
            if cls.__name__
            else f"{keyword} : {underlying}"
        )
        lines = [f"{title} {{"]
        for member_name, member in cls.__members__.items():
            value = hex(member.value) if keyword == "flag" else str(member.value)
            lines.append(f"    {member_name} = {value},")
        lines.append("};")
        return "\n".join(lines)


class _EnumArray:
    """`ENUM[n]` — reads ``n`` members, or to the terminator when ``n`` is None."""

    def __init__(self, enum_type: type, count: int | str | None):
        self._enum = enum_type
        self._count = count
        self.__type__ = enum_type
        self.__num_entries__ = count
        self.__null_terminated__ = count is None
        self.__size__ = count * enum_type.__size__ if isinstance(count, int) else None
        self.__dynamic__ = not isinstance(count, int)
        self.__alignment__ = enum_type.__alignment__
        suffix = "" if count is None else str(count)
        self.__name__ = f"{enum_type.__name__}[{suffix}]"

    def __call__(self, source):
        if not _is_readable(source):
            return _EnumArrayValue(self, source)

        if isinstance(self._count, str):
            raise ValueError(f"array length expression {self._count} needs a structure context")

        stream = _as_stream(source)
        if self._count is not None:
            values = [self._enum._read_one(stream) for _ in range(self._count)]
        else:
            values = []
            while (value := self._enum._read_one(stream)).value != 0:
                values.append(value)
        return _EnumArrayValue(self, values)

    def dumps(self, values, *, endian: str | None = None) -> bytes:
        """Encode each member and append zero for a terminated array."""
        entries = list(values)
        if isinstance(self._count, int) and len(entries) > self._count:
            raise ArraySizeError(
                f"Expected static array size {self._count}, got {len(entries)} instead."
            )
        if isinstance(self._count, int):
            entries.extend(self._enum.__default__() for _ in range(self._count - len(entries)))
        raw = b"".join(
            self._enum._encode_value(
                value.value if isinstance(value, enum.Enum) else value,
                endian=endian,
            )
            for value in entries
        )
        if self._count is None:
            raw += self._enum._encode_value(0, endian=endian)
        return raw

    def write(self, stream, values, *, endian: str | None = None) -> int:
        """Write the full encoded array in one sink call."""
        return stream.write(self.dumps(values, endian=endian))

    def __default__(self):
        """Return one default member per fixed slot, or an empty terminated array."""
        if not isinstance(self._count, int):
            return []
        return [self._enum.__default__() for _ in range(self._count)]


def _expression_array_type(enum_type: type, expression: str) -> _EnumArray:
    """Build cold metadata for an enum array sized by a record expression."""
    return _EnumArray(enum_type, expression)


class _EnumArrayValue(list):
    """List-shaped enum array value that retains its write type."""

    def __init__(self, array_type: _EnumArray, values):
        super().__init__(values)
        self.__type__ = array_type

    def dumps(self, *, endian: str | None = None) -> bytes:
        return self.__type__.dumps(self, endian=endian)

    def write(self, stream, *, endian: str | None = None) -> int:
        return stream.write(self.dumps(endian=endian))


class _CEnum(_EnumStr, enum.IntEnum, metaclass=_CEnumMeta):
    """An enum whose unknown values stay members of the enum.

    Upstream turns an unrecognised value into a pseudo-member: ``repr`` is
    ``<E: 9>`` and the type is still ``E``. Returning a plain ``int`` instead
    loses the type, which a consumer comparing against the enum would notice.
    """

    @classmethod
    def _missing_(cls, value: object):
        if not isinstance(value, int):
            return None
        pseudo = int.__new__(cls, value)
        pseudo._name_ = None
        pseudo._value_ = value
        pseudo._cstruct_hash = hash((cls, None, value))
        return pseudo

    def __eq__(self, other: object) -> bool:
        """Compare members from different cstruct enum classes as unequal."""
        if isinstance(other, _CEnum) and type(other) is not type(self):
            return False
        return int.__eq__(self, other)

    def __ne__(self, other: object) -> bool:
        result = self.__eq__(other)
        return NotImplemented if result is NotImplemented else not result

    __hash__ = _EnumStr.__hash__


class _CFlag(_EnumStr, enum.IntFlag, boundary=enum.KEEP, metaclass=_CEnumMeta):
    """A flag set that keeps bits it has no name for, as upstream does."""

    @classmethod
    def _missing_(cls, value: object):
        member = super()._missing_(value)
        if member is not None:
            member._cstruct_hash = hash((cls, member.name, member.value))
        return member

    def __eq__(self, other: object) -> bool:
        """Compare members from different cstruct flag classes as unequal."""
        if isinstance(other, _CFlag) and type(other) is not type(self):
            return False
        return int.__eq__(self, other)

    def __ne__(self, other: object) -> bool:
        result = self.__eq__(other)
        return NotImplemented if result is NotImplemented else not result

    __hash__ = _EnumStr.__hash__


def _make_enum_class(
    name: str,
    members: dict[str, int],
    is_flag: bool,
    size: int = 4,
    little_endian: bool = True,
    signed: bool = False,
    owner=None,
    base_type=None,
    owner_ref=None,
) -> type:
    """Build one enum or flag class with upstream's semantics.

    Built here rather than with ``enum.IntEnum`` directly in Rust because the
    behaviour that has to match is Python-level: ``__str__``, the handling of
    values with no declared name, and being callable on a stream.

    ``size`` and ``little_endian`` are what make the last of those possible —
    an enum type has to know how many bytes to read.
    """
    base = _CFlag if is_flag else _CEnum
    built = base(name, members)
    built.__size__ = size
    built.__dynamic__ = False
    built.__alignment__ = getattr(base_type, "__alignment__", size)
    if owner_ref is None and owner is not None:
        # Namespace factories enter here from Python. Registry-built classes
        # receive a native weak reference, keeping this cold module import out
        # of ordinary target startup.
        from weakref import ref

        owner_ref = ref(owner)
    built.__cstruct_owner_ref__ = owner_ref
    built.__type__ = base_type
    built.__byteorder__ = "little" if little_endian else "big"
    built.__signed__ = signed
    _fix_alias_members(built)
    for member in built.__members__.values():
        member._cstruct_hash = hash((built, member.name, member.value))
    return built


def _fix_alias_members(cls: type) -> None:
    """Give duplicate-valued declarations their own named member.

    Current upstream deliberately differs from stdlib enums here. It invokes
    the same repair after class construction so ``B = 2`` and ``C = 2`` keep
    distinct identities and names even though they compare equal by value.
    """
    if len(cls._member_names_) == len(cls._member_map_):
        return
    for name, member in list(cls._member_map_.items()):
        if name == member.name:
            continue
        new_member = int.__new__(cls, member.value)
        new_member._name_ = name
        new_member._value_ = member.value
        new_member._cstruct_hash = hash((cls, name, member.value))
        type.__setattr__(cls, name, new_member)
        cls._member_names_.append(name)
        cls._member_map_[name] = new_member
        cls._value2member_map_[member.value] = new_member


__all__ = ["_CEnum", "_CFlag", "_EnumArray", "_EnumArrayValue", "_make_enum_class"]
