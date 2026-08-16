"""``Flag``, re-exported from the package root.

rs2's cstruct flags derive from ``_CFlag`` (an ``IntFlag`` with ``boundary=KEEP``
and cstruct's string behaviour); a loaded ``flag F : uint8 { … }`` has ``_CFlag``
in its MRO. That is the class upstream calls
``dissect.cstruct.types.flag.Flag``.
"""

from __future__ import annotations

from dissect.cstruct import _CFlag

Flag = _CFlag

__all__ = ["Flag"]
