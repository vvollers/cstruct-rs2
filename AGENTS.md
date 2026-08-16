# Working on cstruct-rs2

`cstruct-rs2` is a Rust reimplementation of
[`dissect.cstruct`](https://github.com/fox-it/dissect.cstruct). It ships under
the upstream distribution name and the upstream API, but decodes binary
structures with native read plans instead of generated Python readers. The point
is `target-query` throughput.

It is a replacement, not a wrapper. It never imports or calls upstream at
runtime. Anything it does not support raises an explicit error.

## Layout

| Path | What lives there |
| --- | --- |
| `crates/cstruct-rs2-core/` | Lexer, parser, schema, expressions, plans, decoder. No Python. Safe Rust only. |
| `crates/cstruct-rs2-py/` | PyO3 bindings and the native Python-facing types. |
| `crates/cstruct-rs2-simd/` | Vector kernels. Currently scalar; kept until a measured win justifies unsafe code. |
| `python/dissect/cstruct/` | Thin Python package, CPU import guard, compatibility helpers. Nothing here may run once per decoded field. |
| `crates/*/tests/` | The test suite that ships with this tree: lexer, parser, schema, layout, plans, readers, expressions, hostile input, nesting depth. |

`artifacts/` is where build and measurement output goes. It is ignored and never
committed.

Reference documents at the repository root:
[README.md](README.md) (what it is, how to install it),
[COMPATIBILITY.md](COMPATIBILITY.md) (per-test result against the upstream
suite), [BENCHMARK_AND_VERIFICATION.md](BENCHMARK_AND_VERIFICATION.md)
(plugin-by-artifact timings and adjudicated incompatibilities),
[TEST_ARTIFACTS.md](TEST_ARTIFACTS.md) (the forensic inputs behind those runs),
[CUSTOM_TYPES.md](CUSTOM_TYPES.md), and
[INTENTIONAL_RESOURCE_LIMITS.md](INTENTIONAL_RESOURCE_LIMITS.md).

## Build and check

Development and release builds target CPython 3.13 on Linux x86-64-v3, with the
Rust version pinned in `rust-toolchain.toml`.

```bash
.venv/bin/maturin develop --release

cargo fmt --all -- --check
cargo clippy --release --all-targets --all-features -- -D warnings
cargo test --release --workspace --all-features
.venv/bin/ruff check python
.venv/bin/ruff format --check python
```

There is no CI configuration and no Python test suite in this tree. `cargo test`
is the whole automated check.

## Things that are settled

Do not undo these without an explicit decision; each one is load-bearing.

- **No delegation to upstream.** Production code never imports or calls upstream
  `dissect.cstruct`, and never falls back to it. An upstream checkout kept
  locally for comparison is a read-only reference, never a dependency.
- **`panic = "unwind"`.** A panic must reach the PyO3 boundary and become a
  Python exception. `abort` would turn a corrupt image into a killed
  interpreter.
- **`target-cpu=x86-64-v3` in `.cargo/config.toml`, never `native`.** `native`
  produces a wheel that silently only runs on the build machine. Benchmark
  builds override it through `RUSTFLAGS`.
- **The distribution name is `dissect.cstruct`.** Installing rs2 replaces
  upstream in an environment. Never both.
- **Input is hostile.** Definitions and evidence are untrusted. Offset and size
  arithmetic is checked; array counts, nesting depth and dynamic-record probes
  have fixed ceilings. See
  [INTENTIONAL_RESOURCE_LIMITS.md](INTENTIONAL_RESOURCE_LIMITS.md).
- **Decoded values are ordinary Python scalars and lists**, not upstream's
  type-carrying wrappers. This is the reason for most remaining compatibility
  failures and it is deliberate: the wrappers add allocation and dispatch to
  every field read.

## Making a change

Keep the change in the layer that owns the behavior. Parsing and layout belong
in core; anything about Python object identity belongs in the bindings; the
Python package holds glue, not logic.

Write the test first, as a focused Rust test under `crates/*/tests/`. Behavior
visible only through the installed extension has no harness here at the moment;
if you change something in that band, say so plainly in the change rather than
letting it pass unverified.

The reports in this directory were produced by a validation campaign whose
harness — the Python suite, the differential runners against upstream source,
the downstream gates, the `target-query` comparison and the benchmark tooling —
is not in this tree. They are a record of what was measured, not something a
checkout re-runs. Anything asserting a compatibility or speed claim beyond what
`cargo test` covers has to rebuild that ground first.

Two results stay unacceptable however they are obtained: a faster build that
produces different records, and a compatibility fix that slows the production
workload.

Before measuring anything, read the timing method at the end of
[BENCHMARK_AND_VERIFICATION.md](BENCHMARK_AND_VERIFICATION.md) — fresh
processes, alternating implementation order, exact-output checks before timing.
That method is part of what the existing numbers mean, and a new measurement
taken any other way is not comparable to them.

## Writing comments and documentation

This applies to every comment, docstring, rustdoc block and Markdown page here,
in Rust and Python, in source and in tests.

**Write for a competent maintainer who did not write this code.** Assume they
read Rust and Python and understand pointers, buffers and exceptions. Do not
assume parser theory, CPython internals, LLVM, or the C struct layout rules.
Explain a domain term the first time it matters, next to the code that needs it.

**Say what the code cannot say for itself:** intent, why this approach over the
obvious alternative, what must be true for it to be correct, which odd inputs
are handled deliberately, and what breaks elsewhere if someone changes it.
Never restate the code — if a comment and the line below it say the same thing,
delete the comment.

**Compare against upstream.** This is an alternative implementation, so the most
useful thing a comment can often say is how upstream does it and why this
differs. Do that wherever the behavior is deliberately the same, deliberately
different, or arrived at differently.

```rust
/// Bitfields are masked, never sign-extended.
///
/// Upstream reads a bitfield through its `BitBuffer`, which returns the masked
/// bits and does not widen them, so `int16 b : 12` over `0xa7f` is 2687 rather
/// than -1409. C would sign-extend here; upstream does not, and upstream is the
/// behaviour this implementation matches.
```

Upstream's source and the `dissect.*` packages that consume it are the only
external references you may cite, and cite them concretely — a module, a class,
a function, the shape of a definition.

**No archaeology.** Documentation stands on its own, as if this were greenfield.
A reader should never need to know how it was built. So: no references to
planning or results documents or their identifiers, no anecdotes about test runs
or sample data, no performance numbers from a particular run, no phase numbers
or backlog IDs, and no "this used to be broken" — unless the current shape is
genuinely inexplicable without it, and then name the trap, not who fell into it.
Keep the reasoning, drop the history. Rewrite

> Returning integers made every volume fail to open, while every unit test still
> passed (see the results log, M-018).

as

> A consumer that reads `.value` on a field needs a real enum member; a plain
> `int` does not have one.

The second tells a maintainer what to preserve. The first tells them a story
they cannot check.

**Tone.** Direct technical English, short paragraphs, a concrete example over an
abstract description. Avoid the register that reads as generated: no "it's worth
noting that", no "leverage", no "robust and performant", no piled-up adjectives,
no three-item list where one clause would do, no closing summary that repeats
the paragraph above it. Write like you are explaining it to the colleague at the
next desk.

**Length is proportional to complexity, and no longer.** A one-line helper with
an obvious name gets a one-line comment or none. A function that resolves array
lengths from expressions against a scope gets several paragraphs and probably an
example. Long-form prose — a module header, a design note — should be patient
and step by step, one idea per paragraph, built up from what the reader already
knows. Do not compress it into a dense block that is technically complete and
unreadable.

**Accuracy is not optional.** Every statement must be true of the code as it is
now. If you describe a branch, read the branch. If you name a default, find the
default. If you say upstream does something, look at upstream's source. A
confident wrong comment is worse than no comment: it will be believed and it
will be propagated. When you change code, reread the comments around it — an
outdated comment is a defect, not untidiness.

**Tests.** The name says what is checked. The docstring says why it is worth
checking and what would break if it regressed.

```python
def test_a_char_array_is_one_bytes_object_not_a_list() -> None:
    """`char[n]` is a single `bytes`, and `wchar[n]` a single `str`.

    Upstream produces one value per array rather than a list of one-character
    values. A consumer that reads a filename out of a record expects `b"name"`,
    not `[b"n", b"a", b"m", b"e"]`, and the two compare unequal.
    """
```

Where a test encodes a deliberate difference from upstream, say so and say why.
Where a test exists because a construct is easy to get subtly wrong, name the
trap.
