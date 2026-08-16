"""The classic hex dump, and the ANSI colours it paints with.

``target-query inspect`` reaches this; no decode path does, so it is written for
readability rather than speed.

The colour names are upstream's and are part of the public surface: they are
re-exported from the package root and from ``dissect.cstruct.utils``, and
consumers import them from either. They are defined *here* rather than in
``utils`` because ``hexdump`` is here and ``utils`` imports from the package root
— defining them the other way round would make that import circular.

# The layout is pinned by tests, exactly

Eight-digit offset, two spaces, eight bytes, an extra space, eight more bytes,
two extra spaces, then the text column. Every byte carries its own trailing
space, so the group and column separators are *additional* spaces rather than a
different join. A short final line is padded so the text column still lines up.
"""

from __future__ import annotations

import os
import string
from collections.abc import Iterator
from typing import Any

COLOR_RED = "\033[0;31m"
COLOR_GREEN = "\033[0;32m"
COLOR_YELLOW = "\033[0;93m"
COLOR_BLUE = "\033[0;34m"
COLOR_PURPLE = "\033[0;35m"
COLOR_CYAN = "\033[0;36m"
COLOR_WHITE = "\033[0;37m"
COLOR_BLACK = "\033[0;30m"
COLOR_GREY = "\033[0;90m"

COLOR_RED_BOLD = "\033[1;31m"
COLOR_GREEN_BOLD = "\033[1;32m"
COLOR_YELLOW_BOLD = "\033[1;33m"
COLOR_BLUE_BOLD = "\033[1;34m"
COLOR_PURPLE_BOLD = "\033[1;35m"
COLOR_CYAN_BOLD = "\033[1;36m"
COLOR_WHITE_BOLD = "\033[1;37m"
COLOR_BLACK_BOLD = "\033[1;30m"
COLOR_GREY_BOLD = "\033[1;90m"

COLOR_BG_RED = "\033[1;41m\033[1;37m"
COLOR_BG_GREEN = "\033[1;42m\033[1;37m"
COLOR_BG_YELLOW = "\033[1;43m\033[1;37m"
COLOR_BG_BLUE = "\033[1;44m\033[1;37m"
COLOR_BG_PURPLE = "\033[1;45m\033[1;37m"
COLOR_BG_CYAN = "\033[1;46m\033[1;37m"
COLOR_BG_WHITE = "\033[1;47m\033[1;30m"

COLOR_CLEAR = "\033[0m"
COLOR_CLEAR_BOLD = "\033[1;0m"

#: Bytes rendered as themselves in the text column.
PRINTABLE = string.digits + string.ascii_letters + string.punctuation + " "

#: How the default palette classifies a byte: NUL is noise, printable text is
#: what the eye is usually hunting for, non-ASCII bytes are data, and remaining
#: control bytes are structure. This is current upstream's ``HUMAN_COLORS``
#: policy from ``dissect.cstruct.util``.
_DEFAULT_COLORS = (COLOR_GREY, COLOR_WHITE_BOLD, COLOR_GREEN, COLOR_YELLOW)


def _byte_color(byte: int) -> str:
    if byte == 0:
        return _DEFAULT_COLORS[0]
    if chr(byte) in PRINTABLE:
        return _DEFAULT_COLORS[1]
    if byte & 0x80:
        return _DEFAULT_COLORS[2]
    return _DEFAULT_COLORS[3]


def _hexdump_lines(data: bytes, offset: int, prefix: str, pretty: bool) -> Iterator[str]:
    """Yield one formatted line per 16 bytes.

    Layout, which the tests pin exactly: eight-digit offset, two spaces, eight
    bytes, an extra space, eight more bytes, two extra spaces, then the text
    column. Every byte carries its own trailing space, so the group and column
    separators are *additional* spaces rather than a different join.
    """
    for chunk_start in range(0, len(data), 16):
        chunk = data[chunk_start : chunk_start + 16]

        hex_part = ""
        text_part = ""
        for index in range(16):
            if index < len(chunk):
                byte = chunk[index]
                char = chr(byte) if chr(byte) in PRINTABLE else "."
                if pretty:
                    color = _byte_color(byte)
                    hex_part += f"{color}{byte:02x}{COLOR_CLEAR} "
                    text_part += f"{color}{char}{COLOR_CLEAR}"
                else:
                    hex_part += f"{byte:02x} "
                    text_part += char
            else:
                # Pad a short final line so the text column still lines up.
                hex_part += "   "
            if index == 7:
                hex_part += " "

        yield f"{prefix}{offset + chunk_start:08x}  {hex_part}  {text_part}"


def _autoskip(lines: list[str], data: bytes) -> list[str]:
    """Collapse runs of repeated all-NUL lines to a single ``*``.

    Only *interior* lines collapse: the first and last line of a dump are always
    shown, because the edges are where a reader orients themselves. A run of one
    interior line is left alone -- replacing one line with one ``*`` hides
    information and saves nothing.
    """
    is_null = [all(byte == 0 for byte in data[i * 16 : i * 16 + 16]) for i in range(len(lines))]

    out: list[str] = []
    index = 0
    while index < len(lines):
        interior = 0 < index < len(lines) - 1
        if not (is_null[index] and interior):
            out.append(lines[index])
            index += 1
            continue

        run_end = index
        while run_end < len(lines) - 1 and is_null[run_end]:
            run_end += 1

        out.append(lines[index])  # keep the first line of the run
        if run_end - index > 1:
            out.append("*")
        index = run_end

    return out


def _hexdump(
    data: bytes,
    *,
    palette: list[tuple[int, str]] | None = None,
    offset: int = 0,
    prefix: str = "",
    pretty: bool = False,
    autoskip: bool = False,
) -> Iterator[str]:
    """Yield current upstream's palette-aware 16-byte lines.

    A palette is a sequence of ``(byte_count, ANSI_colour)`` pairs. The count
    is consumed across line boundaries, which is how ``dumpstruct`` paints each
    field by its observed byte width without inspecting the data again.
    """
    if palette:
        palette = palette[::-1]

    if pretty and palette:
        raise ValueError(
            "Cannot use argument 'pretty' in combination with 'palette', please pick one"
        )

    remaining = 0
    active: str | None = None
    in_null_run = False
    in_collapsed_null_run = False
    last_offset = len(data) - 16

    for line_offset in range(0, len(data), 16):
        values = ""
        chars: list[str] = []

        for column in range(16):
            if not active and palette:
                remaining, active = palette.pop()
                while remaining == 0:
                    if not palette:
                        active = ""
                        break
                    remaining, active = palette.pop()
                values += active
            elif active and column == 0:
                values += active

            if line_offset + column >= len(data):
                values += "  "
            else:
                byte = data[line_offset + column]
                character = chr(byte)
                printable = character if character in PRINTABLE else "."

                if active:
                    values += f"{byte:02x}"
                    chars.append(active + printable + COLOR_CLEAR_BOLD)
                elif pretty:
                    color = _byte_color(byte)
                    values += f"{color}{byte:02x}{COLOR_CLEAR}"
                    chars.append(color + printable + COLOR_CLEAR)
                else:
                    values += f"{byte:02x}"
                    chars.append(printable)

                remaining -= 1
                if remaining == 0:
                    active = None
                    if palette is not None:
                        values += COLOR_CLEAR_BOLD

                if column == 15 and palette is not None:
                    values += COLOR_CLEAR_BOLD

            values += " "
            if column == 7:
                values += " "

        if (
            autoskip
            and 0 < line_offset < last_offset
            and data[line_offset : line_offset + 16] == b"\x00" * 16
        ):
            if in_null_run:
                if not in_collapsed_null_run:
                    yield "*"
                    in_collapsed_null_run = True
                continue
            in_null_run = True
        else:
            in_null_run = False
            in_collapsed_null_run = False

        yield f"{prefix}{offset + line_offset:08x}  {values:48s}  {''.join(chars)}"


def hexdump(
    data: bytes,
    palette: list[tuple[int, str]] | None = None,
    offset: int = 0,
    prefix: str = "",
    output: str = "print",
    pretty: bool | None = None,
    autoskip: bool = False,
) -> Any:
    """Print or return a classic hex dump.

    Reached by ``target-query inspect``, never by a decode path, so it is
    written for readability. ``palette`` and ``offset`` stay positional because
    upstream's callers pass them that way.

    ``pretty`` defaults to colour when printing, and honours ``NO_COLOR``
    (https://no-color.org). Colour is suppressed for the ``string`` and
    ``generator`` outputs unless asked for explicitly, since those are usually
    fed to something that will not render escapes.
    """
    if output not in {"print", "generator", "string"}:
        raise ValueError(
            f"Invalid output argument: {output!r} (should be 'print', 'generator' or 'string')."
        )

    if pretty is None:
        pretty = output == "print" and not palette and os.environ.get("NO_COLOR") is None
    elif pretty and os.environ.get("NO_COLOR") is not None:
        pretty = False

    lines = list(
        _hexdump(
            data,
            palette=palette,
            offset=offset,
            prefix=prefix,
            pretty=pretty,
            autoskip=autoskip,
        )
    )

    if output == "generator":
        return iter(lines)
    if output == "string":
        return "\n".join(lines)

    print("\n".join(lines))
    return None


__all__ = [
    "COLOR_BG_BLUE",
    "COLOR_BG_CYAN",
    "COLOR_BG_GREEN",
    "COLOR_BG_PURPLE",
    "COLOR_BG_RED",
    "COLOR_BG_WHITE",
    "COLOR_BG_YELLOW",
    "COLOR_BLACK",
    "COLOR_BLACK_BOLD",
    "COLOR_BLUE",
    "COLOR_BLUE_BOLD",
    "COLOR_CLEAR",
    "COLOR_CLEAR_BOLD",
    "COLOR_CYAN",
    "COLOR_CYAN_BOLD",
    "COLOR_GREEN",
    "COLOR_GREEN_BOLD",
    "COLOR_GREY",
    "COLOR_GREY_BOLD",
    "COLOR_PURPLE",
    "COLOR_PURPLE_BOLD",
    "COLOR_RED",
    "COLOR_RED_BOLD",
    "COLOR_WHITE",
    "COLOR_WHITE_BOLD",
    "COLOR_YELLOW",
    "COLOR_YELLOW_BOLD",
    "PRINTABLE",
    "hexdump",
]
