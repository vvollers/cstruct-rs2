"""The ``dissect.cstruct.types`` subpackage.

Upstream splits its type implementations across this package, and consumers
import from it directly -- ``dissect.target.helpers.protobuf`` does
``from dissect.cstruct.types.base import BaseType``. rs2 implements its types in
Rust and has no equivalent split, but the import path is part of the surface
whether or not the layout behind it matches.

The general rule this follows: a name that is never *called* still has to be
*importable*, because a consumer's module-level imports run when its module is
loaded. A missing submodule is not a missing feature -- it is an ImportError
that takes the importing module with it.

The submodules here re-export from the package root. None of them define a type;
defining one would give consumers two same-named classes and break
``isinstance`` silently.
"""

from __future__ import annotations

from dissect.cstruct.types import base, enum, flag, pointer, structure
from dissect.cstruct.types.base import Array, ArrayType, BaseArray, BaseType, MetaType
from dissect.cstruct.types.enum import Enum
from dissect.cstruct.types.flag import Flag
from dissect.cstruct.types.pointer import Pointer
from dissect.cstruct.types.structure import Field, Structure, StructureMetaType

__all__ = [
    "Array",
    "ArrayType",
    "BaseArray",
    "BaseType",
    "Enum",
    "Field",
    "Flag",
    "MetaType",
    "Pointer",
    "Structure",
    "StructureMetaType",
    "base",
    "enum",
    "flag",
    "pointer",
    "structure",
]
