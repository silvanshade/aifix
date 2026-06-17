# Metrics

## current

### Rust source and tests

| metric                                      | value |
| ------------------------------------------- | ----: |
| Rust source files under `crates/aifix/src/` |    10 |
| Rust source lines under `crates/aifix/src/` | 5,854 |
| Integration test files under `tests/`       |     1 |
| Rust `#[test]` functions under `tests/`     |     7 |
| Test fixture files under `tests/fixtures/`  |     1 |
| Benchmark files under `benches/`            |     1 |
| Fuzz manifest files                         |     1 |
| Fuzz target source files                    |     1 |

Current line counts are from observable files only:

| file                                   | lines |
| -------------------------------------- | ----: |
| `crates/aifix/src/adapter.rs`          | 1,141 |
| `crates/aifix/src/batch.rs`            |   700 |
| `crates/aifix/src/config.rs`           |   448 |
| `crates/aifix/src/digest.rs`           |   865 |
| `crates/aifix/src/error.rs`            |   224 |
| `crates/aifix/src/explain.rs`          |   308 |
| `crates/aifix/src/lib.rs`              |    95 |
| `crates/aifix/src/main.rs`             |   743 |
| `crates/aifix/src/model.rs`            |   827 |
| `crates/aifix/src/render.rs`           |   503 |
| `crates/aifix/Cargo.toml`              |    39 |
| `crates/aifix/tests/pipeline_cli.rs`   |   581 |
| `crates/aifix/benches/ingest.rs`       |    81 |
| `fuzz/Cargo.toml`                      |    22 |
| `fuzz/fuzz_targets/ingest.rs`          |    68 |

### Tests

Current integration tests cover pipeline and batch CLI behavior through `tests/pipeline_cli.rs`:

* clippy compiler-message JSONL to JSON digest;
* TypeScript text diagnostics to markdown guidance;
* LSP diagnostics to compact JSON digest;
* custom batch profile execution through a real executable;
* bounded rejection of over-limit custom batch stdout;
* strict rejection of non-UTF-8 batch extra args;
* rejection of non-file project `aifix.toml` candidates.

Before this documentation pass, observed verification included cargo fmt, cargo build, cargo clippy, nextest with 15 tests passed, and CLI smoke checks for pipeline, explain, and completions.
Final gates are intentionally left to the orchestrator after docs and manifest hashes are refreshed.

### Benchmarks

Benchmark target file: `crates/aifix/benches/ingest.rs`.

Benchmark function registered with Criterion:

* `clippy fixture parse and digest`.

No measured timing baseline is recorded here.
This document should not invent local timings or thresholds without an observed benchmark result.

### Fuzzing

Fuzz manifest: `fuzz/Cargo.toml`.

Fuzz target source: `fuzz/fuzz_targets/ingest.rs`.

The target feeds arbitrary byte input through UTF-8 lossy conversion, attempts all declared protocols, and builds a digest for any parse that succeeds.

## designed direction

* Track diagnostic ingestion throughput after an observed benchmark baseline is accepted.
* Track maximum digest size and sample truncation behavior with representative tool outputs.
* Track adapter coverage by protocol: clippy JSON, TypeScript text, LSP JSON, normalized aifix JSON, and generic/nushell text.
* Track fuzz corpus growth and crash regressions from the `ingest` target.

## open decision

* No benchmark warning or failure thresholds are set.
* No fuzz corpus size or runtime target is set.
* No compact JSON size budget is set.
* No maximum supported diagnostic count has been accepted as a performance budget.
