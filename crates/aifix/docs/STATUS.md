# Status

## current

`aifix` is the Rust CLI crate for turning tool diagnostics into an agent-friendly digest.
The crate contains package metadata, protocol adapters, the normalized model, direct diagnostic and native-fix batch execution, bounded LSP code-action orchestration, config discovery, digest construction, typed errors, deterministic local explanation metadata, rendering, the CLI entry point, integration coverage, unit coverage in core modules, a Criterion ingestion benchmark hook, and a cargo-fuzz ingestion target.

* Crate: `aifix` at `crates/aifix`, with library and binary both named `aifix`.
* Edition: Rust 2024 through the workspace and fuzz package manifest.
* Publishing: disabled through workspace package metadata.
* Runtime dependencies: workspace `camino`, `clap`, `clap_complete`, `directories`, `serde`, `serde_json`, `thiserror`, `toml`, `tree-sitter`, and `tree-sitter-rust`; Unix builds also use workspace `exacl`, `rustix`, and `xattr`.
* Dev dependencies: workspace `criterion` for the `ingest` bench target and `proptest`.
* Current source files under `src/`: `adapter.rs`, `batch.rs`, `cache.rs`, `config.rs`, `digest.rs`, `error.rs`, `explain.rs`, `lib.rs`, `lsp_fix.rs`, `main.rs`, `mcp.rs`, `model.rs`, `render.rs`, `signature.rs`, and `syntax.rs`.
* Public modules in `lib.rs`: `adapter`, `batch`, `cache`, `config`, `digest`, `error`, `explain`, `mcp`, `model`, `render`, `signature`, and `syntax`.
* CLI commands: `pipeline`, `batch`, `explain`, `config paths`, `config profiles`, `mcp`, and `completions <shell>`.
* CLI diagnostic gate options: `pipeline` and `batch` support `--fail-on-diagnostics` with repeated `--expected-code <CODE>` allow-list entries; the command renders the digest first and fails only when diagnostics outside the allow-list remain.
* CLI native-fix option: `batch --fix` runs a profile-owned native fix command before a fresh residual diagnostic pass; the Rust built-in uses `cargo clippy --fix --allow-dirty`, and configured profiles may select a distinct nonzero-output protocol.
* CLI LSP option: `batch --code-actions` applies bounded diagnostic-correlated code actions before the residual diagnostic pass; Rust defaults to `rust-analyzer`, and configured profiles own server and safety settings including exact command allowlists.
* Current integration tests: `crates/aifix/tests/pipeline_cli.rs`.
* Current fixtures: `crates/aifix/tests/fixtures/clippy.jsonl`, `crates/aifix/tests/fixtures/agda.txt`, and the compiled deterministic stdio server source `crates/aifix/tests/fixtures/fake_lsp.rs`.
* Current benchmark hook: `crates/aifix/benches/ingest.rs`, registering `clippy fixture parse and digest`.
* Current fuzz hook: `fuzz/fuzz_targets/ingest.rs`, registered by `fuzz/Cargo.toml` as target `ingest`.

Current integration tests in `pipeline_cli.rs` cover:

* clippy JSON pipeline output as JSON digest;
* Agda text pipeline and auto-detection output as JSON digests, including direct CLI diagnostic spans and status-only success output;
* Agda batch profile execution through a real `agda` executable when available, including expected-code diagnostic gating;
* TypeScript text pipeline output as markdown guidance;
* LSP JSON pipeline output as compact JSON digest;
* custom batch command execution through a real executable;
* successful auto-detected parsing of custom batch stdout above the 1 MiB retention threshold;
* bounded rejection at an explicit per-stream processing budget;
* CLI, selected-profile, auto-profile, and root output-budget precedence;
* MCP `maxOutputBytes` schema and dispatch;
* native-fix capability discovery, explicit unsupported-profile errors, configured `fix_argv`, and MCP post-fix residual diagnostics;
* deterministic CLI and MCP LSP sessions covering text, direct and nested allowlisted commands, command-scoped edit rejection and rollback, transactional multi-file edits, ordered combined mutation, versioned, unversioned, and unopened-document residuals, full and incremental document synchronization, transient retries, ambiguity, stale publications and versions, concurrent source changes, malformed edits and protocol envelopes, blocked, flooding, and oversized server messages, unsupported profiles, and non-convergence;
* incremental UTF-8 boundary acceptance and malformed-byte rejection;
* strict rejection of non-UTF-8 batch extra args;
* rejection of non-file project `aifix.toml` candidates.

Review hardening currently implemented:

* direct process execution without shell expansion;
* strict UTF-8 conversion for batch extra args;
* 1 MiB per-stream in-memory retention with private spill files and a configurable 1 GiB default processing budget;
* incremental UTF-8 validation and record-oriented cargo JSONL and text parsing;
* parseable nonzero diagnostic output can still render a digest;
* malformed structured-looking `auto` input is rejected at the structured boundary;
* TypeScript and LSP adapters validate blank fields and invalid or reversed ranges;
* Agda direct CLI parsing accepts same-line and multi-line diagnostic header locations, preserves normalized multi-line span end positions, and treats known status/progress-only output as zero diagnostics;
* digest dedupe excludes raw payload identity while preserving raw payloads for full JSON evidence.

## designed direction

The crate should stay a small diagnostic and tool-orchestration adapter, not a replacement for the underlying tools.
It should normalize tool output, preserve invocation metadata, run only explicit profile-owned native fixes, group related diagnostics, and render the same digest shape for pipeline and batch modes.

Out of scope for this crate:

* network lookups while explaining diagnostic codes;
* hiding nonzero tool exits when diagnostics are still parseable;
* project-specific Agda source-policy sweeps, including multi-root orchestration or `--without-K` checks for a particular repository.
* maintaining compatibility aliases for old CLI or data-model names.

## open decision

* No CI regression threshold is recorded for the measured ingestion baseline.
* No policy is recorded for when diagnostic suggestions are safe to apply.
* No committed shell completion artifacts are maintained; completions are generated on demand by `aifix completions <shell>`.
* The package name remains `aifix`; collision risk is documented as a naming concern, not a compatibility requirement.
