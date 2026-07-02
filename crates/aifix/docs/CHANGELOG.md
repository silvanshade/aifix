# Changelog

## current

### Crate package and CLI shell

* Added `crates/aifix/Cargo.toml` for library and binary targets named `aifix`.
* Added `src/lib.rs` with public module declarations for the diagnostic adapter crate.
* Added `src/main.rs` for the clap-based CLI entry point covering pipeline, batch, explain, config paths, and shell completion generation.

### Crate-local diagnostic core

* Added protocol adapters in `src/adapter.rs` for normalized aifix JSON, clippy and rustc JSONL, Agda text, TypeScript text, LSP JSON, and generic/nushell text.
* Added normalized serde-compatible model types in `src/model.rs`.
* Added batch profile execution in `src/batch.rs` using direct argv invocation and captured stdout, stderr, cwd, and exit code.
* Added digest construction in `src/digest.rs` for semantic deduplication, source and severity counts, source/code grouping, invocation preservation, and sampled diagnostics.
* Added typed library errors in `src/error.rs`.
* Added deterministic local code-family explanations in `src/explain.rs`.
* Added configuration discovery in `src/config.rs` for user and nearest project configuration merging.
* Added rendering in `src/render.rs` for JSON, compact JSON, and Markdown.

### Review hardening

* Batch execution now preserves direct process boundaries without shell routing.
* Batch capture is bounded to 1 MiB per stream before UTF-8 conversion and invocation retention.
* Batch extra args are rejected when they are not valid UTF-8.
* Nonzero tool exits with parseable diagnostics can produce a digest; unparsable nonzero output remains a process error.
* `auto` protocol rejects malformed structured-looking cargo JSON, complete JSON, LSP, or native `aifix` payloads instead of falling back to generic text.
* TypeScript and LSP adapters reject blank required fields, and LSP rejects malformed or reversed ranges.
* Digest duplicate identity excludes preserved raw payloads and uses normalized source, code, severity, message, spans, and suggestions.
* Compact JSON omits raw diagnostic payloads and captured stdout/stderr bodies while retaining invocation metadata and byte counts.

### Integration coverage

* Added and expanded `tests/pipeline_cli.rs` with end-to-end CLI scenarios including:
  + clippy JSON pipeline to JSON digest;
  + Agda text pipeline and auto detection to JSON digests;
  + Agda batch profile execution through a real `agda` executable when available;
  + TypeScript text pipeline to markdown guidance;
  + LSP JSON pipeline to compact JSON digest;
  + custom batch command execution through a real executable;
  + over-limit custom batch stdout rejection;
  + non-UTF-8 batch extra-arg rejection;
  + non-file project config rejection.
* Added `tests/fixtures/clippy.jsonl` as the current Clippy fixture shared by integration and benchmark paths.
* Added `tests/fixtures/agda.txt` as the Agda CLI text fixture shared by pipeline integration coverage.

### Benchmark and fuzz hooks

* Added `benches/ingest.rs` with a Criterion benchmark named `clippy fixture parse and digest` and recorded the first local baseline in `docs/METRICS.md`.
* Added `fuzz/Cargo.toml` for an `aifix-fuzz` package and declared an `ingest` cargo-fuzz target.
* Added `fuzz/fuzz_targets/ingest.rs` to feed arbitrary UTF-8-lossy input through all declared protocols and digest construction when parsing succeeds.
* Added `docs/OPTIMIZATION.md` to separate measured performance knowledge, trend direction, and open optimization decisions from API behavior.

## designed direction

* Keep changelog entries crate-local: source modules, tests, fixtures, benchmarks, fuzz hooks, and CLI behavior owned by `crates/aifix`.
* Record measured benchmark numbers only after an observed run writes a baseline or maintainers explicitly accept one.
* Record breaking CLI or model changes as clean cutovers, not compatibility shims.

## open decision

* No release history exists yet for published artifacts because publishing is disabled in the workspace.
* No benchmark regression threshold has been accepted.
* No completion scripts are committed as release artifacts; they are generated on demand.
