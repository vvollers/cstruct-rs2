# cstruct-rs2

`cstruct-rs2` is a Rust implementation of
[`dissect.cstruct`](https://github.com/fox-it/dissect.cstruct). It keeps the
same Python package name and the familiar `dissect.cstruct` API, but parses
binary structures with native read plans instead of generated Python readers.
Its main job is to reduce the time Dissect spends decoding records during
`target-query`.

This is an alternative implementation, not a wrapper around the upstream
package. It does not import or call upstream at runtime. Unsupported behavior
raises an explicit error.

## Relationship to upstream

`dissect.cstruct` is a [Fox-IT](https://github.com/fox-it) project and part of
the [Dissect](https://github.com/fox-it/dissect) framework. This project is not
affiliated with Fox-IT and is not endorsed by them. Report anything wrong with
it here, not on upstream's tracker — upstream did not write this code and
cannot fix it.

The reason to say that plainly is the shared distribution name. Installing this
wheel needs no change anywhere in a Dissect environment, which is the point,
but it also means the environment reports `dissect.cstruct` either way and pip
shows one package where two implementations exist. `build_info()` is what
answers the question of which one is installed; see
[installing it in place of upstream](#install-it-in-place-of-upstream-cstruct).

Upstream is the specification. Its source and its test suite define what
correct behavior is here, down to exception types and the wording of error
messages, and [COMPATIBILITY.md](COMPATIBILITY.md) records test by test where
this implementation matches and where it differs on purpose. That is a reading
relationship only — it holds at development time and leaves nothing behind in
the installed package.

The licenses differ. Upstream is Apache-2.0; this implementation is MIT, in
[LICENSE](LICENSE). Where anything is carried over from upstream rather than
written against it, upstream's notice travels with it.

## Supported system

The release build is intentionally narrow:

- CPython 3.13;
- Linux on x86-64-v3 or newer processors; and
- Rust 1.97.1 when building from source.

The x86-64-v3 requirement includes AVX2, BMI2 and FMA. The wheel format cannot
express that CPU floor, so the package checks the processor before loading the
native extension. An older or unverifiable machine gets an `ImportError`
instead of an illegal-instruction crash.

## Install from a release

Each release carries a wheel, a source distribution and a `SHA256SUMS` file,
built from the tagged commit by `.github/workflows/release.yml`.

```bash
gh release download --repo vvollers/cstruct-rs2 \
  --pattern '*.whl' --pattern 'SHA256SUMS'

sha256sum --check --ignore-missing SHA256SUMS
```

`gh` is used rather than a download URL because a release tag contains a `+`,
which a link has to percent-encode.

Releases also carry a GitHub build attestation, which ties the file to the
workflow run and the commit that produced it:

```bash
gh attestation verify dissect_cstruct-*.whl --repo vvollers/cstruct-rs2
```

Then install the wheel over whatever `dissect.cstruct` the target environment
has:

```bash
/path/to/dissect-env/bin/python -m pip install \
  --force-reinstall --no-deps dissect_cstruct-*.whl
```

Release wheels are built inside a `manylinux_2_28` image, so they need glibc
2.28 or newer — RHEL 8, Debian 10, Ubuntu 18.10. That floor is separate from the
CPU floor above, which the package checks at import.

## Build a wheel

Building from source is the alternative to a release download; the result is the
same artifact. Install CPython 3.13 and a Rust toolchain. The repository pins
the required Rust version in `rust-toolchain.toml`.

From this directory:

```bash
python3.13 -m venv .venv
.venv/bin/python -m pip install --upgrade pip 'maturin>=1.7,<2'

mkdir -p artifacts/package/distributions
export SOURCE_DATE_EPOCH=315532800
.venv/bin/maturin build --release --locked \
  --interpreter .venv/bin/python \
  --out artifacts/package/distributions
```

The wheel is written to `artifacts/package/distributions/`. Release builds use
the fixed `x86-64-v3` target from `.cargo/config.toml`; they are not tuned for
the machine that happened to build them.

`maturin sdist` produces a source distribution from the same tree. Two builds of
one commit are byte-identical given the same toolchain: `SOURCE_DATE_EPOCH` is
pinned above, `--locked` fixes the dependency versions, and the target CPU comes
from `.cargo/config.toml` rather than from the build host.

## Install it in place of upstream cstruct

The wheel deliberately uses the upstream distribution name
`dissect.cstruct`. Install one implementation or the other in an environment,
never both.

Replace `/path/to/dissect-env` with the Python environment used by Dissect:

```bash
/path/to/dissect-env/bin/python -m pip install \
  --force-reinstall --no-deps \
  artifacts/package/distributions/dissect_cstruct-*.whl
```

That glob assumes one wheel in the directory. If earlier builds are still there,
name the file the build you just made actually wrote.

Confirm that the native implementation is active:

```bash
/path/to/dissect-env/bin/python - <<'PY'
import importlib.metadata
from dissect.cstruct import build_info

print(importlib.metadata.version("dissect.cstruct"))
print(build_info())
PY
```

`build_info()` is provided by rs2. It reports the version, source identity and
CPU baseline of the installed native module. If a later package operation
installs upstream `dissect.cstruct`, it replaces this wheel because both have
the same distribution identity.

Dissect itself needs no configuration change. Existing imports and commands
remain the same:

```bash
target-query -f walkfs /path/to/disk-image.E01
```

Ordinary Python use also follows the upstream API:

```python
from dissect.cstruct import cstruct

cs = cstruct("struct Header { uint32 magic; uint16 count; };")
header = cs.Header(b"\x78\x56\x34\x12\x03\x00")

assert header.magic == 0x12345678
assert header.count == 3
assert bytes(header) == b"\x78\x56\x34\x12\x03\x00"
```

## Versions and dependency pins

The version is `4.8.0+rs2.1`. The public part, `4.8.0`, names the upstream line
this build implements; `+rs2.N` counts builds of this implementation against
that line. A resolver reads only the public part, so `4.8.0` is what dependency
solving sees.

That number was chosen so the pins the ecosystem actually writes accept it:

| Requirement shape                   | Accepts `4.8.0+rs2.1` |
| ----------------------------------- | --------------------- |
| `dissect.cstruct>=4,<5`             | yes                   |
| `dissect.cstruct>=4.0.dev,<5.0.dev` | yes                   |
| `dissect.cstruct>=4.8.dev7,<5`      | yes                   |
| `dissect.cstruct>=5.0.dev,<6.0.dev` | **no**                |

It also sorts above the newest upstream release, so `pip install -U` on a
consumer package finds nothing newer and leaves this wheel in place. An
explicit `pip install dissect.cstruct`, a `--force-reinstall`, or a lock file
naming an exact upstream version still replaces it.

The last row is the one to watch. Upstream's development line is tagged
`5.0.dev`, and consumers move to a `>=5.0.dev,<6.0.dev` pin as they adopt it.
No single version satisfies both that and the `<5` bound above, so a build for
the 5.0 line will be a separate rs2 release rather than a change to this one.
Match the release to whichever line your Dissect environment pins.

One mismatch is easier to be told than to discover: the version and the
behavior do not describe the same upstream point. `4.8.0` reads as the 4.x
line, but the behavior is validated against upstream's development source, and
[COMPATIBILITY.md](COMPATIBILITY.md) names the commit. A consumer pinning `<5`
accepts this wheel on the strength of a 4.x number and gets development-line
behavior. Nothing in the version says so.

Only the local segment changes between builds of one upstream line. Because a
resolver ignores it, a new `+rs2.N` can never break an environment that already
resolved against `4.8.0`. `.github/scripts/check_release_version.py` enforces
that before a release is built: it derives the pin shapes from the version's own
major number and fails the run if the version has lost its local segment or
falls outside them.

There is no PyPI release. The `dissect.cstruct` name there belongs to upstream,
and a local version segment like `+rs2.1` cannot be uploaded to PyPI in any
case. Wheels come from this repository's releases, or from a build of this
tree.

## Compatibility

The public definition language, native readers and writers, streams and
cursors, exceptions, namespaces, structures, unions, enums, flags, pointers
and custom types cover the current Dissect consumer surface. The latest source
suite used for validation is upstream commit
[`46b850fa9cd023a9de583149caa5e9ba31254cf1`](https://github.com/fox-it/dissect.cstruct/commit/46b850fa9cd023a9de583149caa5e9ba31254cf1).

Against that suite, rs2 passes **459 of 476 applicable tests (96.4%)**. The
complete upstream control passes all 691 tests; 215 of those inspect upstream's
Python compiler, runtime annotations or separate tools and do not apply to a
native replacement.

Seventeen applicable tests remain different:

- sixteen expect upstream-specific Python value wrappers or eager nested-union
  proxies; and
- one requires a misspelled upstream error message.

Decoded values, normal record serialization and cursor movement already match
for the wrapper cases. rs2 returns ordinary Python scalars and lists instead of
putting another Python object around every decoded value. That difference is
kept out of the common read path because the wrappers add allocations and
dispatch to real `target-query` workloads.

[COMPATIBILITY.md](COMPATIBILITY.md) has the node-by-node result: every upstream
test with its outcome, the frozen identities both implementations were tested
at, and why each excluded test is out of scope.

## Expected `target-query` speed

Across the 1,317 artifact/function cells where both implementations produced
identical output, the time-weighted per-plugin command speedup is **1.448×**.

Whole-artifact commands — one invocation with every compatible function enabled
— were exact on nine of the seventeen targets. They range from 1.277× on
`ubuntu-defaced-2024` (51 functions, and the longest workload in the campaign at
over an hour per source run) to 2.620× on `nps-xbox1` (36 functions). The
representative `nps-casper-rw` image, with 50 compatible functions in one
command, measured 1.524×.

Per-plugin commands show larger gains when binary structure decoding is a
substantial part of the work.

The practical rule is simple:

- expect little change when a plugin is dominated by filesystem scanning,
  decompression, database queries or text parsing;
- expect a useful improvement on mixed filesystem and operating-system work;
  and
- expect the largest gains from plugins that decode many cstruct records.

Speed is reported only where source cstruct and rs2 produced equal output and
completed equal work. See
[BENCHMARK_AND_VERIFICATION.md](BENCHMARK_AND_VERIFICATION.md) for every plugin,
artifact, timing pair and adjudicated incompatibility.

## What has been tested

The repository carries the Rust tests under `crates/*/tests/`, which cover the
lexer, parser, schema, layout, bounds checks, read plans, decoder, expressions
and hostile input. Run them with `cargo test`.

The rest of the validation was a separate campaign whose retained result is the
three reports in this directory. Its harness — the Python suite, the
differential runners, the downstream gates and the measurement tooling — is not
part of this tree, so those reports are evidence of what was measured rather
than something a checkout reproduces.

What they record:

- Values, value types, exceptions and final cursor positions were compared
  against the current upstream source running in a separate interpreter. The
  complete upstream suite was staged out of the upstream checkout before pytest
  ran, so it could not import the source package instead of rs2.
- Downstream source suites exercised formats such as FFS, Regf, NTFS, event
  logs, ETL and Cobalt Strike consumers.
- The `target-query` comparison covered 17 disk or reference targets and 1,491
  artifact/function cells. 1,317 produced byte-identical output; only those
  contribute a speed number. The other 174 stay in the matrix as differences
  rather than being dropped, as do empty functions, aliases, registration
  differences and failed invocations.
- Full-artifact timing used one command with every compatible function enabled,
  never a sum of separate plugin timings. Exact-output checks ran before any
  speed measurement, on fresh processes, alternating implementation order, two
  executions per implementation. Large images were allowed to finish; timeouts
  and partial streams were not counted.
- The forensic-input suite covered Windows, Linux, filesystems and containers.
  At the pinned `dissect.target` revision it produced records for 222 of 336
  concrete plugin exports (66.07%).

[COMPATIBILITY.md](COMPATIBILITY.md) holds the per-test result,
[BENCHMARK_AND_VERIFICATION.md](BENCHMARK_AND_VERIFICATION.md) the timings and
the method behind them, and [TEST_ARTIFACTS.md](TEST_ARTIFACTS.md) every image,
reference pack and payload used, with the functions each one exercised.

## Working on the project

Read [AGENTS.md](AGENTS.md) before editing code. It covers the layout, the
checks, the settled decisions and the standard for comments and documentation.

The directories are:

| Path                       | Purpose                                                                                                      |
| -------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `crates/cstruct-rs2-core/` | Python-independent lexer, parser, schema, plans and decoder. Safe Rust only.                                 |
| `crates/cstruct-rs2-py/`   | PyO3 bindings and the native Python-facing types.                                                            |
| `crates/cstruct-rs2-simd/` | Optional vector kernels; currently kept scalar until an optimization justifies unsafe code.                  |
| `python/dissect/cstruct/`  | Thin Python package, import guard and compatibility helpers. Nothing here should run once per decoded field. |

For a local development build and the ordinary checks:

```bash
.venv/bin/maturin develop --release

cargo fmt --all -- --check
cargo clippy --release --all-targets --all-features -- -D warnings
cargo test --release --workspace --all-features
.venv/bin/ruff check python
.venv/bin/ruff format --check python
```

Keep changes close to the layer that owns the behavior, and add a focused Rust
test first. Production code must never import or call upstream `dissect.cstruct`
at runtime, whatever it is being compared against.

A change to the ordinary decode path also needs exact `target-query` output
checks and a serial performance comparison before it can be trusted. That
harness is no longer in this tree, so re-establishing those numbers means
rebuilding it. Do not accept a faster result with different records, or a
compatibility improvement that slows the production workload.

Two deeper references matter when you touch decoding:
[CUSTOM_TYPES.md](CUSTOM_TYPES.md) for reader callbacks, nesting and cursor
behavior, and
[INTENTIONAL_RESOURCE_LIMITS.md](INTENTIONAL_RESOURCE_LIMITS.md) for the array,
nesting and dynamic-record ceilings that untrusted input runs into.
