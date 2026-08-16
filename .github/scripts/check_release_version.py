"""Refuse to release a version that would stop being a drop-in replacement.

A resolver reads only the *public* part of a version. PEP 440 says a local
version label -- everything after the ``+`` -- is ignored when a candidate is
matched against a requirement like ``dissect.cstruct>=4,<5``. That is the whole
reason this project can carry its own build counter: ``4.8.0+rs2.1`` and
``4.8.0+rs2.7`` satisfy exactly the same pins, because a resolver compares
``4.8.0`` in both cases.

So the release version has two halves with different jobs:

``4.8.0``
    The upstream line this build implements. Changing it changes which pins
    accept the wheel, and is a decision about which Dissect environments the
    release is for.

``+rs2.N``
    This implementation's build counter. Bumping it is invisible to dependency
    solving and can never break an installed environment.

This script holds that split in place. It runs before anything is built and
fails the release if the version has lost its local segment (which would make an
rs2 build indistinguishable from an upstream release of the same number), or if
its public part does not satisfy the pin shapes the ecosystem writes for the
major line it claims. Those shapes are derived from the version itself rather
than hard-coded, so a future 5.x release is checked against the 5.x pins without
anyone editing this file.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import NoReturn

from packaging.specifiers import SpecifierSet
from packaging.version import InvalidVersion, Version

# `4.8.0+rs2.1`. The public part is a full three-component release so that it
# sorts unambiguously against upstream's own numbers, and the local part names
# this implementation and counts builds within the line.
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+\+rs2\.\d+$")

REPO_ROOT = Path(__file__).resolve().parents[2]


def fail(message: str) -> NoReturn:
    print(f"::error::{message}", file=sys.stderr)
    sys.exit(1)


def workspace_version() -> str:
    manifest = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    try:
        return manifest["workspace"]["package"]["version"]
    except KeyError:
        fail("Cargo.toml has no [workspace.package] version")


def released_versions() -> list[Version]:
    """Every previously released version, read from the `v*` tags."""
    tags = subprocess.run(
        ["git", "tag", "--list", "v*"],
        capture_output=True,
        text=True,
        check=True,
        cwd=REPO_ROOT,
    ).stdout.split()

    versions = []
    for tag in tags:
        try:
            versions.append(Version(tag.removeprefix("v")))
        except InvalidVersion:
            # A tag that is not a version is somebody else's business.
            continue
    return versions


def main() -> None:
    requested = os.environ["REQUESTED_VERSION"].strip()
    declared = workspace_version()

    # The version is not injected into the tree by this workflow. A release is
    # built from the commit exactly as it was reviewed, which is what lets two
    # builds of one commit come out byte-identical. The dispatch input is a
    # confirmation that the operator knows what is about to ship, not a source
    # of truth.
    if requested != declared:
        fail(
            f"requested {requested!r} but Cargo.toml declares {declared!r}. "
            "Commit the version bump first; this workflow does not edit the tree."
        )

    if not VERSION_RE.match(declared):
        fail(f"{declared!r} is not of the form MAJOR.MINOR.PATCH+rs2.N")

    version = Version(declared)

    # The pins the Dissect ecosystem writes, expressed in terms of the major
    # line this version claims. Both shapes appear in practice: consumers use
    # the plain bound for runtime dependencies and the `.dev` bound for their
    # development extras.
    major = version.release[0]
    required_pins = [
        f">={major},<{major + 1}",
        f">={major}.0.dev,<{major + 1}.0.dev",
    ]

    for pin in required_pins:
        if version not in SpecifierSet(pin):
            fail(
                f"{declared} does not satisfy {pin}, so a consumer pinning that "
                "range would not accept this wheel. The release would not be a "
                "drop-in replacement."
            )

    # Going backwards would let an older build overwrite a newer one in an
    # environment that already has this package.
    previous = released_versions()
    if previous:
        newest = max(previous)
        if version <= newest:
            fail(f"{declared} is not newer than the last released version {newest}")

    tag = f"v{declared}"

    print(f"version:  {declared}")
    print(f"public:   {version.public}  (what a resolver matches on)")
    print(f"local:    {version.local}  (ignored by dependency solving)")
    print(f"tag:      {tag}")
    print("satisfies:")
    for pin in required_pins:
        print(f"  {pin}")

    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
        handle.write(f"version={declared}\n")
        handle.write(f"public={version.public}\n")
        handle.write(f"major={major}\n")
        handle.write(f"next_major={major + 1}\n")
        handle.write(f"tag={tag}\n")


if __name__ == "__main__":
    main()
