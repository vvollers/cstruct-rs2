"""A Rust implementation of ``dissect.cstruct``.

This package installs under the ``dissect.cstruct`` import path so that code
written against upstream works unchanged. Nothing here imports the upstream
package: falling back to it would turn a missing feature into something much
harder to notice than an error.

**Nothing in this module runs per record.** Everything on a decode path -- the
parser, the compiled plan, field reads, stream handling -- is in the Rust
extension. What lives here is the surface around it: the exception classes, the
enum and flag classes a definition's types are built from, the generated struct
classes, and the hexdump helpers. If a function here starts being called once
per field, it is in the wrong place.

Anything upstream provides that this implementation does not is listed in
:data:`UNSUPPORTED` and raises ``NotImplementedError`` when reached, rather than
being absent and failing later as an ``AttributeError``.

# How this file is organised

It is a facade. The implementation is split across private modules by subject,
and this module imports from them and re-exports the names consumers use:

===================== =======================================================
``_bases``            ``BaseType`` and ``MetaType``, which everything else needs
``_streams``          turning a buffer or a file into something with a cursor
``_hexdump``          the hex dump and the ANSI colour names
``_enum_types``       ``IntEnum``/``IntFlag`` classes with cstruct's semantics
``_struct_types``     the generated class in front of a compiled structure
``_value_types``      what a type object returns when called with a value
``_namespace``        definition-time helpers bound onto the ``cstruct`` class
===================== =======================================================

The split is by subject rather than by layer, so a change to how enums behave
touches one file. Import order matters in two places and both are commented
where they happen: ``_namespace`` is imported for its side effect of patching the
``cstruct`` class, and the ``dissect.cstruct.cstruct`` submodule has to be loaded
at the very end.
"""

from __future__ import annotations

import enum as _enum
from typing import Any, NoReturn

# The wheel tag says only ``x86_64`` even though the native module is compiled
# for x86-64-v3. This pure-Python check must remain before every native import:
# moving it below `_cstruct_rs2` can turn a useful ImportError into SIGILL on an
# older analyst or worker host. Importing `_cpu` performs the one-time check.
from dissect.cstruct import _cpu as _cpu_guard  # noqa: F401
from dissect.cstruct import _cstruct_rs2
from dissect.cstruct._bases import BaseType, MetaType

# Every import below re-exports: this module is the package's public face and the
# implementation lives in the private modules beside it. `F401` is suppressed
# per-block rather than per-name, because "unused here" is the *point* of a
# facade -- the names exist so that `from dissect.cstruct import X` resolves.
# ``ArrayType`` and ``Field`` are bound here as well as in the extension because
# upstream exposes both under ``dissect.cstruct.types`` and its tests import
# them. They are the classes the Rust side already returns -- ``cs.uint32[2]``
# is an ``ArrayType`` -- so this is a binding, not a new type.
from dissect.cstruct._cstruct_rs2 import (
    ArrayType,
    Field,
    Record,
    ScalarType,
    StructType,
    cstruct,
)
from dissect.cstruct._dumpstruct import dumpstruct

# ---------------------------------------------------------------------------
# Private names kept reachable at the package root.
#
# These aliases are not public upstream API. Keeping the canonical helper
# objects importable here avoids creating parallel enum, structure, or value
# classes for code that inspects rs2 internals. Standard-library modules remain
# implementation details and are not re-exported.
# ---------------------------------------------------------------------------
from dissect.cstruct._enum_types import (  # noqa: F401  # noqa: F401
    _CEnum,
    _CEnumMeta,
    _CFlag,
    _EnumArray,
    _EnumStr,
    _make_enum_class,
)
from dissect.cstruct._hexdump import (  # noqa: F401  # noqa: F401
    _DEFAULT_COLORS,
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
    _autoskip,
    _byte_color,
    _hexdump_lines,
    hexdump,
)

# Imported for its side effect as well as for ``Void``: the module binds the
# definition-time helpers (``cdef``, ``_make_enum``, ``_make_pointer`` and the
# rest) onto the ``cstruct`` class. Nothing later in this file depends on the
# order, but the helpers must exist before a consumer loads a definition.
from dissect.cstruct._namespace import (  # noqa: F401
    Void,
    _cdef,
    _loadfile,
    _make_enum,
    _make_flag,
    _make_int_type,
    _make_pointer,
    _make_struct,
    _make_union,
    _resolve,
    _scalar_shape,
    _type_name,
)
from dissect.cstruct._streams import _as_stream, _is_readable, _take  # noqa: F401
from dissect.cstruct._struct_types import (  # noqa: F401
    Structure,
    StructureMetaType,
    Union,
    UnionProxy,
    _make_struct_class,
    _StructArray,
)
from dissect.cstruct._value_types import (  # noqa: F401
    _BIG_ENDIAN,
    _VALUE_CLASSES,
    _make_value,
    _value_class,
)

__all__ = [
    "ArrayType",
    "Field",
    "Record",
    "ScalarType",
    "StructType",
    "build_info",
    "cstruct",
    "dumpstruct",
    "fallback_report",
    "fallback_reset",
    "fallback_total",
]


#: Upstream API this implementation does not provide, and why.
#:
#: Being listed here is better than being absent: reaching for one of these
#: raises ``NotImplementedError`` naming the feature, rather than an
#: ``AttributeError`` that looks like a typo.
#:
#: Note that ``dumps`` is *not* here. Encoding a record is supported, because
#: callers use it while reading -- see ``Record.dumps``.
UNSUPPORTED: dict[str, str] = {
    "write": "encoding straight to a file object",
    "stubgen": "type stub generation",
    "ctypes": "ctypes interoperability",
    "ctypes_type": "ctypes interoperability",
}


def _unsupported(name: str) -> NoReturn:
    """Refuse a name this implementation does not provide, and say what it was.

    Named rather than bare, so the caller can tell a deliberate omission from a
    misspelling.
    """
    reason = UNSUPPORTED.get(name, "not implemented")
    raise NotImplementedError(
        f"dissect.cstruct.{name} is not available in this implementation: {reason}"
    )


def __getattr__(name: str) -> Any:
    """Turn a reach for unimplemented surface into an explicit failure.

    Module ``__getattr__`` runs only when normal lookup fails, so this costs
    nothing for anything that exists.
    """
    if name in UNSUPPORTED:
        _unsupported(name)
    raise AttributeError(f"module 'dissect.cstruct' has no attribute {name!r}")


def dumps(obj: Any) -> bytes:
    """Encode a record to bytes.

    Module-level for parity with upstream, which exposes the same call on the
    record, on its type, and here. All three reach the same Rust encoder.
    """
    try:
        return obj.dumps()
    except AttributeError:
        raise TypeError(f"cannot encode {type(obj).__name__}: not a cstruct record") from None


__all__ += ["dumps"]


def build_info() -> dict[str, Any]:
    """Describe the native build: CPU baseline, AVX-512, abi3, core version.

    Which CPU baseline and which CPython the extension was built for decide
    where the wheel runs at all, so it is worth being able to ask.
    """
    return _cstruct_rs2.build_info()


def fallback_report() -> list[dict[str, Any]]:
    """Return the fast-path miss report, most frequent first.

    Each row names a type and why decoding it missed a fast path. The ordering
    is the useful part: whatever is at the top accounts for the most misses.
    Correctness never depends on this -- a miss is decoded correctly by the
    general path.
    """
    return _cstruct_rs2.fallback_report()


def fallback_total() -> int:
    """Total fast-path misses recorded in this process."""
    return _cstruct_rs2.fallback_total()


def fallback_reset() -> None:
    """Clear the fallback counters.

    They are process-wide, so reset before a run that should be reported on its
    own.
    """
    _cstruct_rs2.fallback_reset()


# ---------------------------------------------------------------------------
# Compatibility aliases
#
# Names code imports from the module directly, for annotations and `isinstance`
# checks rather than to call. Aliases, not new classes: binding a second class to
# one of these names would make an `isinstance` check against the other one
# return False without any error.
# ---------------------------------------------------------------------------

#: Used by `dissect.ntfs` as `isinstance(item, Enum)` to tell an enum member
#: from a plain int.
#:
#: `enum.Enum` rather than a bespoke class, because the members this
#: implementation produces are `IntEnum` and `IntFlag` members, and both derive
#: from `enum.Enum`. A bespoke base would make that `isinstance` return False
#: and silently change how `dissect.ntfs` looks up attributes.
Enum = _enum.Enum

#: A `char[n]` field's value type.
CharArray = bytes

__all__ += ["BaseType", "CharArray", "Enum", "MetaType", "Structure"]
__all__ += ["StructureMetaType"]
__all__ += ["hexdump"]
__all__ += ["Void"]
__all__ += ["_make_value"]
__all__ += ["Union", "UnionProxy"]


# ---------------------------------------------------------------------------
# `dissect.cstruct.cstruct` is both a class and a module, and the order matters
#
# Importing a submodule makes it an attribute of its package. Without an
# explicit rebind, `import dissect.cstruct.cstruct` would make a later
# `from dissect.cstruct import cstruct` return the module rather than the
# callable class. Current upstream avoids that ambiguity by loading the
# submodule during package initialisation and exposing the class at the package
# root; this facade follows the same order.
#
# It sits at the very end of the file because the submodule imports the class
# back out of this package, and the class has to exist by then.
#
# `importlib.import_module` rather than a plain `from ... import`: the attribute
# it would bind already exists (it is the class), so the ordinary spelling
# resolves to that and never loads the module at all.
import importlib as _importlib  # noqa: E402

_cstruct_module = _importlib.import_module("dissect.cstruct.cstruct")
# The submodule reads the initial class binding while it imports; this final
# assignment restores that class over CPython's temporary module attribute.
cstruct = _cstruct_module.cstruct  # noqa: F811
