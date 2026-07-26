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
* Batch output now retains 1 MiB per stream in invocation metadata, spills larger complete streams for parsing, and enforces a configurable 1 GiB default processing budget.
* Batch extra args are rejected when they are not valid UTF-8.
* Nonzero tool exits with parseable diagnostics can produce a digest; unparsable nonzero output remains a process error.
* Agda direct CLI parsing now accepts same-line and multi-line diagnostic header locations, preserves multi-line span end positions, and treats status-only success output as zero diagnostics.
* `pipeline` and `batch` gained diagnostic gate options: `--fail-on-diagnostics` fails only for diagnostics whose code is not allowed by repeated `--expected-code <CODE>`, while the rendered digest remains visible.
* `auto` protocol rejects malformed structured-looking cargo JSON, complete JSON, LSP, or native `aifix` payloads instead of falling back to generic text.
* TypeScript and LSP adapters reject blank required fields, and LSP rejects malformed or reversed ranges.
* Digest duplicate identity excludes preserved raw payloads and uses normalized source, code, severity, message, spans, and suggestions.
* Compact JSON omits raw diagnostic payloads and captured stdout/stderr bodies while retaining invocation metadata and byte counts.
* Batch profile discovery now lists `auto`, built-ins, `custom`, and configured profiles with detection metadata across JSON, compact JSON, and Markdown renderings.
* Batch execution now treats omitted profiles and explicit `auto` as the automatic profile, aggregates applicable Rust, TypeScript, Agda, and Nushell diagnostics, and reports per-profile statuses.
* Batch extra args are documented as profile-specific, and `auto` rejects extra args instead of forwarding them ambiguously.
* MCP batch tools now expose profile listing, accept omitted or empty batch profiles, and return structured recovery data for unknown profiles.
* Batch `--fix` and MCP `fix: true` now run profile-owned native automatic fixes before a fresh residual diagnostic pass.
* The Rust profile supplies `cargo clippy --fix --allow-dirty`; configured profiles may declare complete direct-argv `fix_argv`, select a distinct non-automatic `fix_protocol` for nonzero output, and advertise the fix command family through profile discovery.
* Named unsupported fix requests fail with configuration recovery, while automatic runs keep unsupported profiles diagnostic-only.
* Batch `--code-actions` and MCP `codeActions: true` now run a bounded profile-owned stdio LSP session before the final diagnostic invocation.
* The Rust profile defaults to `rust-analyzer`; configured profiles may declare server argv, language ID, source extensions, action kinds, exact command allowlists, iteration caps, and timeouts.
* Automatic code-action mode permits exactly one detected capable mutator and fails preflight before source changes when multiple profiles could mutate.
* Automatic action selection requires one eligible action or one eligible preferred action per diagnostic.
  Direct edits use transactional validation; allowlisted, server-advertised commands may submit one edit through the same path, while out-of-scope or unsafe server edit requests are rejected.
  Multi-file changes stage completely before replacement with rollback on partial failure, current unversioned and unopened-document residuals remain visible, complete session time and server traffic are bounded, stale publications and source changes cannot drive edits, and unsafe edit forms are rejected.

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
  + non-file project config rejection;
  + native Clippy mutation for clean and staged fixtures with post-fix residual diagnostics;
  + configured `fix_argv` execution, custom-command argument independence, distinct fix-output protocol classification, permissive auto-protocol rejection, pre-mutation command validation, and unsupported-profile CLI errors;
  + deterministic fake-LSP CLI and MCP flows for text, allowlisted direct and nested commands, early command responses, command-scoped edits and rollback, transactional multi-file edits, atomic-exchange validation-gap races, ordered native-plus-LSP mutation, post-action versioned, unversioned, and unopened-document residual diagnostics, full and incremental document synchronization, transient retries, ambiguity, stale publications and document versions, concurrent source changes, malformed edits and protocol envelopes, blocked, flooding, and oversized server messages, unsupported profiles, and non-convergence;
* Added `tests/fixtures/clippy.jsonl` as the current Clippy fixture shared by integration and benchmark paths.
* Added `tests/fixtures/agda.txt` as the Agda CLI text fixture shared by pipeline integration coverage.
* Added `tests/fixtures/fake_lsp.rs` as a deterministic stdio language server compiled by integration tests.

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
