"""Parser façade over the native cstruct definition parser.

``CStyleParser`` keeps the constructor and ``parse`` method exposed by current
upstream. Its work is still performed by the Rust parser used by
``cstruct.load``. The native call returns the small amount of parser state a
caller can observe, so a façade parse never tokenises or parses the definition
twice.

Upstream's private recursive-descent methods and Python bytecode compiler are
implementation details and are intentionally absent.
"""

from __future__ import annotations

import re
from typing import Any


def _join_line_continuations(string: str) -> str:
    """Remove C-style backslash-newline continuations before tokenisation."""
    return re.sub(r"\\\n", "", string)


class Parser:
    """Base for definition parser façades bound to one cstruct namespace."""

    def __init__(self, cs: Any) -> None:
        self.cs = cs

    def parse(self, data: str) -> None:
        """Parse *data* into the bound namespace."""
        raise NotImplementedError


class CStyleParser(Parser):
    """Load C-style definitions through the namespace's native parser.

    ``compiled`` is accepted because upstream selects between two Python read
    implementations with it. rs2 has one native compiled-plan path, so the
    value does not select another parser. ``align`` retains its upstream meaning
    and gives structures declared by this call natural alignment.
    """

    def __init__(self, cs: Any, compiled: bool = True, align: bool = False) -> None:
        super().__init__(cs)
        self.compiled = compiled
        self.align = align
        self._data: str | None = None
        self._flags: list[str] = []
        self._conditional_stack: list[object] = []

    def reset(self) -> None:
        """Clear state retained from the previous definition."""
        self._data = None
        self._flags = []
        self._conditional_stack = []

    def parse(self, data: str) -> None:
        """Parse *data* once and retain its remaining configuration flags."""
        self.reset()
        self._data = _join_line_continuations(data)
        metadata = self.cs._parse_definition(data, compiled=self.compiled, align=self.align)
        self._flags = list(metadata["flags"])


__all__ = ["CStyleParser", "Parser"]
