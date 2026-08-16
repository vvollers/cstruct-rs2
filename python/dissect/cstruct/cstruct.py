"""``dissect.cstruct.cstruct`` — the namespace class, at upstream's import path.

Upstream defines the namespace class in ``dissect.cstruct.cstruct``, and code
imports it from there as well as from the package root. Without this module the
name resolves only at the root and those imports fail.

Note that ``dissect.cstruct.cstruct`` is both a module and, at the package root,
a class. Importing the module binds it as an attribute of the package, which
would shadow the class for the rest of the process -- so the package root
re-binds the class after this module is imported.
"""

from __future__ import annotations

from dissect.cstruct import cstruct

__all__ = ["cstruct"]
