"""Render a parsed structure beside the bytes that produced it.

The format follows current upstream's ``dissect.cstruct.util.dumpstruct``:
integer fields use hexadecimal, other values use ``repr``, and each declared
field can colour the bytes it consumed.  Cobalt Strike's ``beacon -t
dumpstruct`` command relies on the field text, so a successful call that emits
only the hex half is not compatible.

Upstream stores declared fields and their values on a generated record class.
This implementation has one native ``Record`` class instead; a private native
helper provides the equivalent diagnostic view without affecting record field
lookup.  The helper is not consulted while decoding.

Upstream reference:
https://github.com/fox-it/dissect.cstruct/blob/46b850fa9cd023a9de583149caa5e9ba31254cf1/dissect/cstruct/util.py#L276-L359
"""

from __future__ import annotations

from enum import Enum
from typing import Any

from dissect.cstruct._cstruct_rs2 import _record_dump_fields
from dissect.cstruct._hexdump import (
    COLOR_BG_BLUE,
    COLOR_BG_CYAN,
    COLOR_BG_GREEN,
    COLOR_BG_PURPLE,
    COLOR_BG_RED,
    COLOR_BG_WHITE,
    COLOR_BG_YELLOW,
    COLOR_BLUE_BOLD,
    COLOR_CLEAR_BOLD,
    COLOR_CYAN_BOLD,
    COLOR_GREEN_BOLD,
    COLOR_PURPLE_BOLD,
    COLOR_RED_BOLD,
    COLOR_WHITE_BOLD,
    COLOR_YELLOW_BOLD,
    hexdump,
)


def _field_name(field: Any) -> str:
    """Return the name from either upstream's or the native field object."""
    name = getattr(field, "_name", None)
    return name if name is not None else field.name


def _record_name(structure: Any) -> str:
    """Name the logical structure rather than the shared native record class."""
    return getattr(structure, "__typename__", structure.__class__.__name__)


def _declared_fields_and_values(structure: Any):
    """Yield direct fields and values in declaration order.

    Anonymous containers are direct fields even though their child names are
    also folded into the outer record.  Upstream reads the hidden container
    attribute; the native record supplies the equivalent value tuple because
    its public lookup intentionally contains only folded names.
    """
    fields, direct_values = _record_dump_fields(structure)
    for index, field in enumerate(fields):
        name = _field_name(field)
        value = direct_values[index]
        yield field, name, value


def _format_value(value: Any, field_name: str) -> str:
    """Use current upstream's compact field-value spelling."""
    if isinstance(value, int) and not isinstance(value, Enum):
        return hex(value)
    if isinstance(value, list):
        # `pprint` is comparatively expensive to import and lists are only
        # inspected when this diagnostic helper is called. Keeping it here
        # preserves ordinary `dissect.cstruct` cold start.
        import pprint

        rendered = pprint.pformat(value)
        if "\n" in rendered:
            rendered = rendered.replace("\n", f"\n{' ' * (len(field_name) + 4)}")
        return rendered
    return repr(value)


def _dumpstruct(
    structure: Any,
    data: bytes,
    offset: int,
    color: bool,
    output: str,
    autoskip: bool,
) -> str | None:
    """Format an already parsed structure and its encoded bytes."""
    palette: list[tuple[int, str]] = []
    colors = [
        (COLOR_RED_BOLD, COLOR_BG_RED),
        (COLOR_GREEN_BOLD, COLOR_BG_GREEN),
        (COLOR_YELLOW_BOLD, COLOR_BG_YELLOW),
        (COLOR_BLUE_BOLD, COLOR_BG_BLUE),
        (COLOR_PURPLE_BOLD, COLOR_BG_PURPLE),
        (COLOR_CYAN_BOLD, COLOR_BG_CYAN),
        (COLOR_WHITE_BOLD, COLOR_BG_WHITE),
    ]
    color_index = 0
    lines = [f"struct {_record_name(structure)}:"]

    for field, name, value in _declared_fields_and_values(structure):
        if getattr(field.type, "anonymous", False):
            continue

        rendered = _format_value(value, name)
        if color:
            foreground, background = colors[color_index % len(colors)]
            palette.append((structure.__sizes__[name], background))
            color_index += 1
            lines.append(f"- {foreground}{name}{COLOR_CLEAR_BOLD}: {rendered}")
        else:
            lines.append(f"- {name}: {rendered}")

    field_text = "\n".join(lines)
    if output == "print":
        print()
        hexdump(data, palette=palette, offset=offset, autoskip=autoskip)
        print()
        print(field_text)
        return None

    return (
        f"\n{hexdump(data, palette=palette, offset=offset, output='string', autoskip=autoskip)}"
        f"\n\n{field_text}"
    )


def dumpstruct(
    obj: Any,
    data: bytes | None = None,
    offset: int = 0,
    color: bool = True,
    autoskip: bool = False,
    output: str = "print",
) -> str | None:
    """Print or return a structure diagnostic.

    ``obj`` may be a generated structure class plus ``data`` or an already
    parsed record.  ``output`` accepts only ``"print"`` and ``"string"``, as
    current upstream does; use :func:`hexdump` directly for generator output.
    """
    if output not in ("print", "string"):
        raise ValueError(f"Invalid output argument: {output!r} (should be 'print' or 'string').")

    if isinstance(obj, type) and data is not None:
        return _dumpstruct(obj(data), data, offset, color, output, autoskip)

    return _dumpstruct(obj, obj.dumps(), offset, color, output, autoskip)


__all__ = ["dumpstruct"]
