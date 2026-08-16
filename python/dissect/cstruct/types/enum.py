"""``Enum``, re-exported from the package root.

rs2's cstruct enums derive from ``_CEnum`` (an ``IntEnum`` with cstruct's string
behaviour, built by the Rust side); a loaded ``enum E : uint8 { … }`` has
``_CEnum`` in its MRO. That is the class upstream calls
``dissect.cstruct.types.enum.Enum``, so that is what this binds.

Note this is *not* the same object as ``dissect.cstruct.Enum``, which is
currently the stdlib ``enum.Enum``. That discrepancy predates this module and is
recorded as a genuine gap rather than papered over here -- binding two different
classes to one name is the failure this package's ``types/base.py`` docstring
warns about, and picking the wrong one silently is worse than leaving it
visible.
"""

from __future__ import annotations

import sys

from dissect.cstruct import _CEnum

#: Upstream gates some enum behaviour on Python 3.11. This build requires a
#: newer Python than that, so it is always true here; it exists because code
#: written against upstream imports it.
PY_311 = sys.version_info >= (3, 11, 0)

Enum = _CEnum

__all__ = ["PY_311", "Enum"]
