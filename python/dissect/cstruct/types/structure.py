"""``Field``, ``Structure`` and ``StructureMetaType``, re-exported from the package root.

A loaded ``union u { ... }`` decodes through the same machinery as a struct --
overlaying members at offset zero is handled when the type is compiled -- but it
is still distinguishable: a union's generated class carries ``Union`` in its
bases, so ``issubclass(cs.U, Union)`` is true and ``issubclass(cs.S, Union)`` is
false for a plain struct.

That distinction is worth keeping. Binding ``Union`` to the shared metaclass
instead would make every struct in the namespace answer true.
"""

from __future__ import annotations

from dissect.cstruct import Field, Structure, StructureMetaType, Union, UnionProxy

__all__ = ["Field", "Structure", "StructureMetaType", "Union", "UnionProxy"]
