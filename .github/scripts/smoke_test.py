"""Prove the built wheel imports and decodes, before it becomes a release.

`cargo test` covers the Rust crates, but nothing in this tree exercises the
installed artifact: the CPU guard, the PyO3 module load, and the Python surface
around it only run once a wheel is on disk. This is the one check that does, so
it stays small and unconditional -- if it cannot pass, there is nothing worth
publishing.

Run against a virtualenv that has the wheel installed. The expected version is
passed in so that a stale wheel left in `dist/` cannot be released under a new
number.
"""

from __future__ import annotations

import importlib.metadata
import sys

from dissect.cstruct import build_info, cstruct


def main() -> int:
    expected = sys.argv[1]

    installed = importlib.metadata.version("dissect.cstruct")
    if installed != expected:
        print(f"installed {installed}, expected {expected}", file=sys.stderr)
        return 1

    info = build_info()
    print(f"version:    {installed}")
    print(f"build_info: {info}")

    # The README's example, so a release cannot ship with the front page wrong.
    cs = cstruct("struct Header { uint32 magic; uint16 count; };")
    header = cs.Header(b"\x78\x56\x34\x12\x03\x00")

    assert header.magic == 0x12345678, header.magic
    assert header.count == 3, header.count
    assert bytes(header) == b"\x78\x56\x34\x12\x03\x00", bytes(header)

    print("decode round-trip: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
