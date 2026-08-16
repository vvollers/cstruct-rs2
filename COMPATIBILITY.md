# cstruct-rs2 compatibility with the upstream source test suite

## Executive summary

The current `cstruct-rs2` release wheel passes **459 of the 476 upstream tests that apply to its public behavior (96.4%)**. Seventeen applicable tests still fail. Sixteen of those failures ask rs2 to wrap otherwise correct decoded values in extra Python objects, or to create parent-linked union proxies. Those objects add allocation and dispatch to the same record-reading path that rs2 is meant to accelerate. A previous complete value-wrapper design made a representative `dissect target-query` `walkfs` workload 36% slower without adding another passing upstream test, so these behaviors remain deliberately deferred.

The remaining applicable failure is only an error-message spelling difference: upstream's test requires `arethmethic`, while rs2 returns the correctly spelled `arithmetic`.

The upstream suite contains another 215 tests that do not describe rs2's public parsing contract. Most inspect the Python lexer/compiler, generated Python code, runtime annotations, or tools that the native implementation replaces. They remain visible below as **Not included**; none are silently removed from the denominator. The source implementation itself passed all 691 tests in the fresh control run.

### Compatibility at a glance

| Report status | Tests | Share | Meaning |
| --- | ---: | ---: | --- |
| Pass | 459 | 96.4% of applicable tests | Public behavior matched the upstream source test. |
| Fail | 17 | 3.6% of applicable tests | A genuine, named behavior difference remains. |
| Not included | 215 | 31.1% of the complete suite | The test asserts upstream-specific architecture or benchmark tooling, not the replacement's public parsing behavior. |
| **Total** | **691** | **100% of source suite** | Every upstream node has exactly one row in this report. |

### Remaining applicable failures

| Code | Capability | Tests | Why it is not implemented |
| --- | --- | ---: | --- |
| G1 | Standalone decoded-value wrappers | 6 | Correct native scalar values would need an additional type-carrying Python object on scalar reads. |
| G2 | Structure/union array subclasses | 4 | Correct lists would need generated wrapper objects carrying `dumps()` and upstream type identity. |
| G3 | Record field value subclasses | 4 | Every decoded scalar and array in a record would need an upstream-style wrapper. |
| G4 | Eager nested-union proxies | 2 | Every relevant nested member would need a parent link and write-through proxy. |
| G5 | Diagnostic spelling | 1 | rs2 does not reproduce upstream's misspelled `arethmethic` message. |
| **Total** |  | **17** | The first four groups are deferred because they add work to ordinary decoding; G5 is a deliberate text difference. |

The 16 G1-G4 nodes do not show wrong decoded numbers, bytes, defaults, or cursor positions. They check the Python class of those values, the presence of wrapper-only methods, or mutation through a nested proxy. This final run tests compatibility only; it does not repeat the separate target-query performance campaign.

### Tests that are not part of the rs2 contract

| Code | Upstream-specific area | Tests | Observed with rs2 | Why it is not included |
| --- | --- | ---: | --- | --- |
| N1 | Python compiler and compiled variants | 86 | 66 failed; 20 not collected | rs2 compiles native plans, not Python bytecode. |
| N2 | Runtime annotations | 89 | 80 failed; 9 not collected | Native type objects do not have upstream's reimported Python class annotations. |
| N3 | Stub generator | 18 | 18 not collected | This is a separate upstream code-generation utility. |
| N4 | ctypes backend | 7 | 7 not collected | rs2 exposes this backend as unsupported and never delegates to upstream. |
| N5 | Internal BitBuffer helper | 1 | 1 not collected | Public bitfield behavior is covered by included tests. |
| N6 | pytest benchmark harness | 14 | 12 passed; 2 failed | Microbenchmark fixtures are not correctness tests or the target-query performance gate. |
| **Total** |  | **215** | **12 passed; 148 failed; 55 not collected** | These rows stay listed so architecture gaps cannot disappear from the accounting. |

N1 combines the `[compiled]` parameter variants, the direct compiler module tests, and five tests that patch or inspect generated Python helpers. N6's two failures are `test_benchmark_parser` and `test_benchmark_lexer_and_parser`: both assign a partially bound function to `cs.add_type`, while rs2 deliberately exposes that native method as read-only. The other twelve benchmark nodes execute successfully, but are still excluded because their assertions concern a microbenchmark harness rather than compatible output.

## What was tested

| Item | Frozen identity |
| --- | --- |
| Upstream repository | [`fox-it/dissect.cstruct`](https://github.com/fox-it/dissect.cstruct) |
| Upstream source revision | [`46b850fa9cd023a9de583149caa5e9ba31254cf1`](https://github.com/fox-it/dissect.cstruct/tree/46b850fa9cd023a9de583149caa5e9ba31254cf1) |
| Upstream distribution | `5.0.dev9+g46b850fa9` |
| Upstream package-tree SHA-256 | `0b2a396b62da82cd79587bf1367d2d41aab10996b51ce508a597e964c0184cca` |
| rs2 distribution | `4.8.0+rs2.1` |
| rs2 wheel SHA-256 | `9e745656d4b91da1c0be7a6622cc781fe444b926f37b319f4b7159df61925559` |
| rs2 native-module SHA-256 | `b36a729cf49a968b1f41dafb7f3f5b1a4aeeade7a6f37c087b8acb8789c3d0c5` |
| rs2 package-tree SHA-256 | `3815711afc2cbbf5e4e249e692c83816ffcbdf10d2aabd121aea428838ac7f37` |
| rs2 source identity | `6d8a3529da05ca6d80e641280e38bb0f8493c01d839f15dfbe8b403bbc800978` |
| Python | 3.13.14 |
| Run date | 2026-08-16 |

The tests were copied out of the upstream checkout before pytest ran. This prevents pytest from adding upstream's source package to `sys.path` and accidentally testing upstream against itself. The runner also verified the package origin, native binary hash, build identity, and source-tree identity before scoring rs2. `PYTHONPATH` and inherited `NO_COLOR` were removed. A separate probe confirmed that both implementations honor `NO_COLOR` when it is deliberately enabled.

### Raw outcomes

| Run | Passed | Failed | Not collected | Total source nodes |
| --- | ---: | ---: | ---: | ---: |
| Upstream source control | 691 | 0 | 0 | 691 |
| rs2, before scope classification | 471 | 165 | 55 | 691 |
| rs2, applicable behavior only | 459 | 17 | — | 476 |

The raw rs2 pass count is 471 because twelve benchmark-harness tests run successfully but are not counted as compatibility behavior. A test that the source control collects but rs2 cannot collect is recorded as `NOT-COLLECTED`, not dropped.

## Reproduction

From `cstruct-rs2/`, with the retained final environments:

```bash
env -u NO_COLOR \
  artifacts/upstream-test-suite-compatibility/final-01/final-env/bin/python \
  tools/test_parity/gate.py \
  --python artifacts/upstream-test-suite-compatibility/final-01/final-env/bin/python \
  --tests ../ecosystem/dissect.cstruct \
  --oracle-python artifacts/upstream-test-suite-compatibility/gate/envs/oracle/bin/python \
  --expected-subject-source . \
  --identity-report artifacts/upstream-source-suite-final/identity.json \
  --results-report artifacts/upstream-source-suite-final/rs2-results.json
```

The retained node-level files are `cstruct-rs2/artifacts/upstream-source-suite-final/oracle-results.json` and `rs2-results.json`. The verified implementation identities are in `identity.json`.

## Per-test compatibility matrix

`Observed with rs2` is pytest's exact node outcome. `Report status` applies the scope decision described above. For a Not included row, `FAILED` or `NOT-COLLECTED` is expected and is not counted as a public behavior failure.

| Upstream test node | Report status | Observed with rs2 | Reason |
| --- | --- | --- | --- |
| `tests/test_align.py::test_align_array[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_array[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_bitfield[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_bitfield[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_dynamic[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_dynamic[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_nested_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_nested_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_pointer[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_pointer[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_struct_array[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_align.py::test_align_struct_array[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_union` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_align.py::test_align_union_tail` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_annotations.py::test_cstruct_type_annotation[BYTE]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[CHAR]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[DWORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT128]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[INT]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[LONG32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[LONG64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[LONGLONG]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[LONG]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[OWORD]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[QWORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[SHORT]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UCHAR]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT128]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[UINT]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ULONG64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ULONGLONG]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ULONG]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[USHORT]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[WCHAR]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[WORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[_BYTE]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[_DWORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[_OWORD]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[_QWORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[_WORD]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__int128]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__int16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__int32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__int64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__int8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__u128]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__u16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__u32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__u64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[__u8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[char]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[double]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[float16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[float]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ileb128]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int128]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int128_t]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int16_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int24]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int32_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int48]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int64_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int8_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[int]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[long]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[short]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[u128]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[u16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[u32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[u64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[u8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uchar]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint128]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint128_t]` | Not included | `NOT-COLLECTED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint16]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint16_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint24]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint32]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint32_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint48]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint64]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint64_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint8]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint8_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uint]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[uleb128]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ulong]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[ushort]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[void]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[wchar]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_annotations.py::test_cstruct_type_annotation[wchar_t]` | Not included | `FAILED` | N2: Upstream runtime annotation machinery. rs2 native type objects have no equivalent Python class body to reimport and inspect. |
| `tests/test_basic.py::test_array_class_name` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_array_of_null_terminated_strings[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_array_of_null_terminated_strings[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_array_of_size_limited_strings[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_array_of_size_limited_strings[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_array_three_dimensional[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_array_three_dimensional[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_cdef` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_cdef_defines` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_compiler_slicing_multiple[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_compiler_slicing_multiple[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_config_flag_nocompile[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_config_flag_nocompile[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_const_type_name_collision` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_constants` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_copy` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_cstruct_bytearray` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_del_const_after_copy` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_deprecation_warnings` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_dumps_write_overload` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_dunder_bytes` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_duplicate_type[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_duplicate_type[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_duplicate_types` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_dynamic_substruct_size` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_endianness` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_half_compiled_struct` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_linked_list` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_load_file[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_load_file[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_load_init` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_load_init_kwargs_only` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_multipart_type_name` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_nested_array_of_variable_size[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_nested_array_of_variable_size[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_read_type_name` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_report_array_size_mismatch` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_reserved_attribute_names` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_reserved_keyword[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_reserved_keyword[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_size_and_aligment` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_type_replace` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_type_resolve` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_typedef` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_basic.py::test_underscores_attribute[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_basic.py::test_underscores_attribute[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_benchmark.py::test_benchmark_attribute_access` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_basic[compiled]` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_basic[interpreted]` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_expression_evaluate` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_expression_parse` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_expression_parse_and_evaluate` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_getattr_constants` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_getattr_typedefs` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_getattr_types` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_lexer` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_lexer_and_parser` | Not included | `FAILED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_parser` | Not included | `FAILED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_union[compiled]` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_benchmark.py::test_benchmark_union[interpreted]` | Not included | `PASSED` | N6: A pytest-benchmark harness, not a correctness contract. Real target-query A/B gates are used for release performance. |
| `tests/test_bitbuffer.py::test_bitbuffer_read` | Not included | `NOT-COLLECTED` | N5: An internal upstream BitBuffer class. Public bitfield behavior is covered by the included bitfield tests. |
| `tests/test_bitfield.py::test_bitfield[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_char[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_char[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_consecutive[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_consecutive[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_dynamic[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_dynamic[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_straddle[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_straddle[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_with_enum_or_flag[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_with_enum_or_flag[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_write[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_write[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_bitfield_write_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_bitfield_write_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_bitfield.py::test_struct_after_bitfield[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_bitfield.py::test_struct_after_bitfield[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_compiler.py::test_generate_array_read` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_bits_read` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_fields_dynamic_after_bitfield[int8]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_fields_dynamic_after_bitfield[uint64]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_fields_dynamic_before_bitfield[int8]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_fields_dynamic_before_bitfield[uint64]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_packed_read` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_packed_read_array` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_packed_read_byte_types` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_packed_read_composite_types` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_packed_read_offsets` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_struct_info` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_struct_info_offsets` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_structure_read` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_generate_structure_read_anonymous` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_optimize_struct_fmt[fields0-4B]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_optimize_struct_fmt[fields1-4B2H]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_optimize_struct_fmt[fields2-B]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_optimize_struct_fmt[fields3-B2H]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_compiler.py::test_optimize_struct_fmt[fields4-B2xH]` | Not included | `NOT-COLLECTED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_ctypes.py::test_ctypes_structure` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type[char-c_char]` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type[char[3]-c_char_Array_3]` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type[int8*-LP_c_byte]` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type[int8-c_byte]` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type[int8[3]-c_byte_Array_3]` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_ctypes.py::test_ctypes_type_exception` | Not included | `NOT-COLLECTED` | N4: The optional ctypes backend is outside rs2's supported backend; it fails explicitly instead of falling back. |
| `tests/test_expression.py::test_dotted_identifier` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_dotted_identifier_not_integer` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_dotted_identifier_unknown_member` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[   7  *     8  -56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[(1 & 1) * 4-4]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[(1 + 2)-3]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[-42--42]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[-~1-2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 & 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 + 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 - 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 - 1--1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 / 1-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 \| 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0 \| 1-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[010ULL-8]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0Xf0 >> 4-15]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x0 << 4-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x0 >> 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x1 << 0-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x1 >> 0-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x1 >> 1-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x1B-27]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0x1b-27]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0xf << 4-240]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[0xf0 >> 4-15]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 % 1-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 % 2-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 & 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 & 1 * 4-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 & 1-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 & 2-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 * 0-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 * 1-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 + 0-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 + 3-4]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 - 0-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 - 3--2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 / 1-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 ^ 0-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 ^ 1-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 ^ 3-2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 \| 1-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 \| 2 \| 4-7]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[1 \| 2-3]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[10l-10]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[10ll-10]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[10ull-10]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[2 / 2-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[21 - B-8]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[3 - 1-2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[3 / 2-1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[4 * 1 + 1-5]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[4 / 2-2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[42 + (-42)-0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[5 % 3-2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[7 * 8-56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[7 *8-56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[7*8-56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[A + 5-13]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[A + B-21]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[\\t7\\t*\\t8\\t-56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[~(A + 5)--14]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression[~1--2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[close-parenthesis]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[double-binary-operator]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[empty-binary-literal]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[empty-expression]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[empty-hex-literal]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[empty-parenthesis]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[invalid-sizeof-expression]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[invalid-sizeof-usage]` | Fail | `FAILED` | G5: The asserted error text contains upstream's misspelling 'arethmethic'; rs2 deliberately spells 'arithmetic' correctly. |
| `tests/test_expression.py::test_expression_failure[missing-parenthesis]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[not-enough-operands]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[open-parenthesis]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[too-many-operands]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[unexpected-character]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[unknown-identifier0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[unknown-identifier1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_expression_failure[unterminated-parenthesis]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_offsetof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_expression.py::test_sizeof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error["unterminated-unexpected end of input]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[#foobar-unknown preprocessor directive]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[#include "missing_end-unexpected end of input]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[#include <missing_end-unexpected end of input]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[#include-unexpected end of input]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error['unterminated-unexpected end of input]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[0888-invalid octal literal]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[0b-invalid binary literal]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[0x-invalid hexadecimal literal]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[@-unexpected character]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error[`-unexpected character]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_error_context` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_line_and_column_tracking` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[   \\t\\n  -types4-values4]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[ -types1-values1]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[""-types69-values69]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token["hello world"-types63-values63]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token["line1\\nline2"-types64-values64]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token["quote: '"-types66-values66]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token["tab\\tseparated"-types65-values65]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#[]-types61-values61]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#[compiled=True]-types62-values62]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#define FOO "hello"-types51-values51]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#define FOO 42-types50-values50]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#define FOO ADCRYPT\\x00-types52-values52]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#define FOO-types49-values49]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#else-types57-values57]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#endif-types58-values58]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#ifdef-types55-values55]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#ifndef-types56-values56]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#include "foo.h"-types59-values59]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#include <stdint.h>-types60-values60]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[#undef-types54-values54]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[%-types36-values36]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[&-types37-values37]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[''-types70-values70]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token['escaped \\\\'quote\\\\''-types68-values68]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token['single quoted'-types67-values67]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[(-types26-values26]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[(0) - 1-types46-values46]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[)-types27-values27]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[*-types31-values31]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[+-types33-values33]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[,-types29-values29]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[--types34-values34]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[-1-types45-values45]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[-types0-values0]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[/-types35-values35]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0-types5-values5]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0755-types18-values18]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0B1100-types17-values17]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0XFF-types15-values15]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0b1010-types16-values16]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0o777-types19-values19]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[0xff-types14-values14]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[1 - 1-types48-values48]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[1.-types21-values21]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[100L-types10-values10]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[100ULL-types13-values13]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[100l-types9-values9]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[100ll-types11-values11]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[100ull-types12-values12]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[1234-types6-values6]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[3.14-types20-values20]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[42U-types8-values8]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[42u-types7-values7]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[:-types30-values30]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[;-types28-values28]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[<<-types41-values41]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[=-types32-values32]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[>>-types42-values42]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[A = 0, B = 1-types80-values80]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[B"hello"-types72-values72]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[UINT32-types76-values76]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[[-types24-values24]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[\\n            #define FOO (1             + 2)\\n            -types53-values53]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[\\n-types3-values3]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[\\t-types2-values2]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[]-types25-values25]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[^-types39-values39]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[_my_var-types75-values75]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[a << b-types43-values43]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[b'abc'-types71-values71]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[b_var-types73-values73]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[c[1] - 1-types47-values47]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[field[69]-types79-values79]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[hello-types74-values74]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[uint32_t bit0:1;-types78-values78]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[uint32_t-types77-values77]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[x >> 2-types44-values44]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[{-types22-values22]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[\|-types38-values38]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[}-types23-values23]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_lexer.py::test_token[~-types40-values40]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_ifdef` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_ifndef` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_ifndef_guard` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_in_struct` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_nested` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_conditional_parsing_error` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_config_flags` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_define` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_define_flag_value` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_dynamic_substruct_size` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_enum_flag_digit_member_name` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_error_context` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_includes` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_multiline_define` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_multiple_declarators` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_nested_structs[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_parser.py::test_nested_structs[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_preprocessor_define_from_enum_in_struct` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_preprocessor_in_struct_body` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_preserve_comment_newlines` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_parser.py::test_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_struct_names` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_typedef_enum` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_typedef_pointer` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_typedef_types` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_parser.py::test_undef` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[alias-stub]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[cstruct-stub]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[define-literals]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[pointer-alias]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[reference other struct array]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[reference other struct]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub[typedef-stub]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_cstruct_stub_empty` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_enum_stub[enum-int8]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_enum_stub[enum]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_enum_stub[flag]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_file_stub` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[anonymous-nested]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[array]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[basic]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[named-nested-array]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[named-nested]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_tools_stubgen.py::test_generate_structure_stub[pointer]` | Not included | `NOT-COLLECTED` | N3: Upstream's Python stub generator, which is a separate code-generation tool rather than cstruct parsing behavior. |
| `tests/test_types_base.py::test_array_size_mismatch` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_base.py::test_array_write_padding` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_base.py::test_custom_array_type[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_base.py::test_custom_array_type[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_base.py::test_eof[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_base.py::test_eof[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_base.py::test_truthy_type` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_array` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_array_write_padding` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_array_write_size_error` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_char.py::test_char_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_custom.py::test_adding_custom_type` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_custom.py::test_custom_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_custom.py::test_custom_deprecated_signature` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_custom.py::test_using_type_in_struct` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_cdef_enum` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_alias` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_anonymous[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_anonymous[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_anonymous_struct[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_anonymous_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_bad_type` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_comments` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_default_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_name[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_enum.py::test_enum_name[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_reference_own_member[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_reference_own_member[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_same_value_different_type[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_same_value_different_type[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_str_repr` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_str_repr_in_struct[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_str_repr_in_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_enum.py::test_enum_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_struct_write[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_enum.py::test_enum_struct_write[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_enum.py::test_enum_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_cdef_flag` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_anonymous[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_anonymous[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_anonymous_struct[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_anonymous_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_operator` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_str_repr` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_str_repr_in_struct[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_str_repr_in_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_struct` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_struct_read[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_flag.py::test_flag_struct_read[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_flag.py::test_flag_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_range` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_be_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_be_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_be_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_be_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_signed_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_types_int.py::test_int_struct_signed[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_int.py::test_int_struct_signed[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_struct_signed_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_int.py::test_int_struct_signed_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_struct_unsigned[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_int.py::test_int_struct_unsigned[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_struct_unsigned_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_int.py::test_int_struct_unsigned_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_be_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_be_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_be_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_be_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_int.py::test_int_unsigned_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_types_leb128.py::test_leb128_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_nested_struct_signed` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_nested_struct_unsigned` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_signed_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_signed_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_types_leb128.py::test_leb128_struct_signed_zero` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_struct_unsigned` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_struct_unsigned_zero` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_unsigned_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_unsigned_read_EOF` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_unsigned_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_types_leb128.py::test_leb128_unsigned_write_amount_written` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_leb128.py::test_leb128_write_negatives` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_be_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_be_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_be_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_be_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_float_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_packed.py::test_packed_float_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_float_struct_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_packed.py::test_packed_float_struct_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_range` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_packed.py::test_packed_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_types_pointer.py::test_pointer` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_array[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_pointer.py::test_pointer_array[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_changing_endian` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_char` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_of_pointer[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_pointer.py::test_pointer_of_pointer[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_operator` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_pointer.py::test_pointer_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_struct_pointer[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_pointer.py::test_pointer_struct_pointer[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_pointer.py::test_pointer_sys_size` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_align_struct_in_struct` | Not included | `FAILED` | N7: Inspection or patching of upstream's generated Python initializer/codegen helpers; rs2 stores native plans instead. |
| `tests/test_types_structure.py::test_cdef_anonymous_nested` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_arrays_pointers_bitfields` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_non_recursive_references_by_name` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_recursive` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_recursive_self_reference` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_referenced_define` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_referenced_define_deduplicated` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_round_trip` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_cdef_struct` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_codegen_hashable` | Not included | `FAILED` | N7: Inspection or patching of upstream's generated Python initializer/codegen helpers; rs2 stores native plans instead. |
| `tests/test_types_structure.py::test_codegen_make_init` | Not included | `FAILED` | N7: Inspection or patching of upstream's generated Python initializer/codegen helpers; rs2 stores native plans instead. |
| `tests/test_types_structure.py::test_structure` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_array_read` | Fail | `FAILED` | G2: The decoded values are correct, but upstream returns a generated Array subclass with type identity and dumps(); rs2 returns a list. |
| `tests/test_types_structure.py::test_structure_array_write` | Fail | `FAILED` | G2: The decoded values are correct, but upstream returns a generated Array subclass with type identity and dumps(); rs2 returns a list. |
| `tests/test_types_structure.py::test_structure_bool` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_changing_endian[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_changing_endian[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_cmp` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_default[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_default[interpreted]` | Fail | `FAILED` | G3: Values and defaults match, but upstream stores BaseType subclasses in records while rs2 stores native Python values. |
| `tests/test_types_structure.py::test_structure_default_dynamic[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_default_dynamic[interpreted]` | Fail | `FAILED` | G3: Values and defaults match, but upstream stores BaseType subclasses in records while rs2 stores native Python values. |
| `tests/test_types_structure.py::test_structure_definition_expressions[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_expressions[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_nested[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_nested[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_newline[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_newline[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_self` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_simple[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_simple[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_simple_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_simple_be[interpreted]` | Fail | `FAILED` | G3: Values and defaults match, but upstream stores BaseType subclasses in records while rs2 stores native Python values. |
| `tests/test_types_structure.py::test_structure_definition_sizes[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_sizes[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_write[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_write[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_write_anonymous` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definition_write_be[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definition_write_be[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_definitions[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_definitions[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_endian_name_conflict[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_endian_name_conflict[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_field_discard[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_structure.py::test_structure_field_discard[interpreted]` | Not included | `FAILED` | N8: A patch hook on an upstream Python scalar constructor. The public discard result and cursor behavior are tested elsewhere. |
| `tests/test_types_structure.py::test_structure_field_duplicate` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_modify` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_partial_initialization` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_repr` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_same_name_method` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_single_byte_field` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_values_dict[compiled]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_values_dict[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_structure.py::test_structure_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_cdef_union` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_codegen_hashable` | Not included | `FAILED` | N7: Inspection or patching of upstream's generated Python initializer/codegen helpers; rs2 stores native plans instead. |
| `tests/test_types_union.py::test_union` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_anonymous_update` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_array_read` | Fail | `FAILED` | G2: The decoded values are correct, but upstream returns a generated Array subclass with type identity and dumps(); rs2 returns a list. |
| `tests/test_types_union.py::test_union_array_write` | Fail | `FAILED` | G2: The decoded values are correct, but upstream returns a generated Array subclass with type identity and dumps(); rs2 returns a list. |
| `tests/test_types_union.py::test_union_bool` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_changing_endian` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_cmp` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_default` | Fail | `FAILED` | G3: Values and defaults match, but upstream stores BaseType subclasses in records while rs2 stores native Python values. |
| `tests/test_types_union.py::test_union_default_dynamic` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_definition` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_definition_anonymous[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_union.py::test_union_definition_anonymous[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_definition_dynamic` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_definition_nested[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_union.py::test_union_definition_nested[interpreted]` | Fail | `FAILED` | G4: Upstream wraps a nested union member in a parent-linked proxy, so later child assignment rebuilds sibling views; rs2 does not create that eager proxy. |
| `tests/test_types_union.py::test_union_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_modify` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_nested_update` | Fail | `FAILED` | G4: Upstream wraps a nested union member in a parent-linked proxy, so later child assignment rebuilds sibling views; rs2 does not create that eager proxy. |
| `tests/test_types_union.py::test_union_partial_initialization` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_partial_initialization_dynamic` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_read_offset` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_repr` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_update` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_union.py::test_union_write_anonymous` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_array_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_struct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_types_void.py::test_void_struct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_void.py::test_void_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_array` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_array_write_padding` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_array_write_size_error` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_be_array` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_be_array_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_be_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_be_write` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_default` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_eof` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_read` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_types_wchar.py::test_wchar_write` | Fail | `FAILED` | G1: The value is correct, but upstream returns a type-carrying scalar wrapper; rs2 returns the native Python scalar. |
| `tests/test_utils.py::test_dumpstruct[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_utils.py::test_dumpstruct[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_dumpstruct_anonymous[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_utils.py::test_dumpstruct_anonymous[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_dumpstruct_enum[compiled]` | Not included | `FAILED` | N1: Upstream Python-compiler behavior. rs2 has one native decode path and truthfully does not claim that upstream's bytecode compiler ran. |
| `tests/test_utils.py::test_dumpstruct_enum[interpreted]` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_autoskip_collapses_middle_null_run` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_autoskip_false_does_not_collapse` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_autoskip_keeps_edge_null_lines` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_autoskip_separate_null_runs` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_autoskip_single_interior_null_line_is_not_collapsed` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_pretty` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_hexdump_pretty_print_conditions` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_pack_unpack` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |
| `tests/test_utils.py::test_swap` | Pass | `PASSED` | P: The public behavior asserted by this source test matches. |

## Reconciliation checks

- Per-test rows: 691.
- Report statuses: 459 Pass + 17 Fail + 215 Not included = 691.
- Oracle control: 691 collected and 691 passed.
- Parity gate problems: 0.
- No test is normalized away, merged with another node, or omitted because its module did not import under rs2.
