"""Reject hosts below the native extension's x86-64-v3 floor.

Python wheel tags can express x86-64 but not the cumulative x86-64-v2/v3/v4
micro-architecture levels. The extension is compiled for v3, so importing it
before checking the host could execute an unsupported instruction in the
dynamic loader or module initializer. This module deliberately contains only
baseline Python code and is imported before the extension.

Linux documents a flag present in ``/proc/cpuinfo`` as both supported by the
hardware and currently made available by the kernel. In particular, Linux
withholds ``avx`` when the XSAVE state needed to execute AVX is unavailable.
The feature names below are the cumulative v2 and v3 sets from the x86-64
psABI; ``pni`` and ``abm`` are Linux's established names for SSE3 and LZCNT.
"""

from __future__ import annotations

import os
import sys

_REQUIRED_FEATURES = (
    ("CMPXCHG16B", frozenset({"cx16"})),
    ("LAHF-SAHF", frozenset({"lahf_lm"})),
    ("POPCNT", frozenset({"popcnt"})),
    ("SSE3", frozenset({"pni", "sse3"})),
    ("SSE4.1", frozenset({"sse4_1"})),
    ("SSE4.2", frozenset({"sse4_2"})),
    ("SSSE3", frozenset({"ssse3"})),
    ("AVX", frozenset({"avx"})),
    ("AVX2", frozenset({"avx2"})),
    ("BMI1", frozenset({"bmi1"})),
    ("BMI2", frozenset({"bmi2"})),
    ("F16C", frozenset({"f16c"})),
    ("FMA", frozenset({"fma"})),
    ("LZCNT", frozenset({"abm", "lzcnt"})),
    ("MOVBE", frozenset({"movbe"})),
    # Linux exposes AVX only after enabling its XSAVE dependency. Requiring the
    # explicit xsave flag as well keeps the error conservative on old kernels.
    ("OS-enabled XSAVE", frozenset({"xsave"})),
)


def _common_cpu_features(cpuinfo: str) -> frozenset[str]:
    """Return features reported for every processor in Linux ``cpuinfo``.

    Hybrid and misconfigured virtual machines can expose different features on
    different logical CPUs. A process may migrate to any of them, so the safe
    contract is their intersection rather than the first CPU or their union.
    """
    feature_sets = []
    for line in cpuinfo.splitlines():
        key, separator, value = line.partition(":")
        if separator and key.strip().lower() in {"flags", "features"}:
            feature_sets.append(frozenset(value.lower().split()))
    if not feature_sets:
        return frozenset()
    return frozenset.intersection(*feature_sets)


def _missing_x86_64_v3_features(cpuinfo: str) -> tuple[str, ...]:
    """Return psABI v2/v3 features absent from at least one logical CPU."""
    common = _common_cpu_features(cpuinfo)
    if not common:
        return ("readable per-CPU feature flags",)
    return tuple(name for name, aliases in _REQUIRED_FEATURES if common.isdisjoint(aliases))


def require_x86_64_v3() -> None:
    """Raise ``ImportError`` before native import unless this is Linux v3.

    Failure is deliberately closed: a missing ``/proc`` mount or an unknown
    architecture is not permission to risk loading a v3 binary. There is no
    environment-variable bypass because that would turn a packaging guard into
    an opt-out switch capable of restoring ``SIGILL``.
    """
    try:
        machine = os.uname().machine.lower()
    except AttributeError:
        machine = "unknown"

    if sys.platform != "linux" or machine not in {"x86_64", "amd64"}:
        raise ImportError(
            "cstruct-rs2 requires Linux x86-64 with the x86-64-v3 feature set; "
            f"this interpreter reports {sys.platform}/{machine}. The native extension "
            "was not loaded."
        )

    try:
        with open("/proc/cpuinfo", encoding="ascii", errors="replace") as stream:
            cpuinfo = stream.read()
    except OSError as error:
        raise ImportError(
            "cstruct-rs2 could not verify the required x86-64-v3 feature set from "
            f"/proc/cpuinfo ({error}). The native extension was not loaded."
        ) from error

    missing = _missing_x86_64_v3_features(cpuinfo)
    if missing:
        raise ImportError(
            "cstruct-rs2 requires the x86-64-v3 feature set; missing or unavailable: "
            f"{', '.join(missing)}. The native extension was not loaded."
        )


# Package initialization imports this module before `_cstruct_rs2`. Keep the
# check here, rather than between imports in the facade, so Python's ordinary
# import ordering itself enforces the safety boundary.
require_x86_64_v3()
