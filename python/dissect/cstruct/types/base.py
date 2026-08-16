"""``BaseType``, ``MetaType`` and the array bases used by Dissect consumers.

All of these are defined in ``dissect.cstruct`` itself; this module exists so
that upstream's import path resolves. Defining them here instead would give a
consumer two distinct classes with the same name, and an ``isinstance`` check
against the wrong one fails silently -- the same failure mode as the two ``KEY``
enum classes that produced ``unsupported operand type(s) for 'in'``.

Native built-in arrays use :class:`ArrayType`.  Consumer-defined types use the
subclassable :class:`BaseArray` protocol, as upstream does, because a plugin may
override how an entire custom array is read.  The two paths stay separate so a
built-in array never performs a Python subclass check while decoding.
"""

from __future__ import annotations

from dissect.cstruct import ArrayType
from dissect.cstruct._bases import Array, BaseArray, BaseType, MetaType

__all__ = ["Array", "ArrayType", "BaseArray", "BaseType", "MetaType"]
