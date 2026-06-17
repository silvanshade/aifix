# Status

## current

`aifix` is the Rust CLI crate for turning tool diagnostics into an agent-friendly digest.
The crate contains package metadata, protocol adapters, the normalized model, direct batch execution, config discovery, digest construction, typed errors, deterministic local explanation metadata, rendering, the CLI entry point, integration coverage, unit coverage in core modules, a Criterion ingestion benchmark hook, and a cargo-fuzz ingestion target.

* Crate: `aifix` at `crates/aifix`, with library and binary both named `aifix`.
* Edition: Rust 2024 through the workspace and fuzz package manifest.
* Publishing: disabled through workspace package metadata.
* Runtime dependencies: workspace `camino`, `clap`, `clap_complete`, `directories`, `serde`, `serde_json`, `thiserror`, and `toml`.
* Dev dependency: workspace `criterion` for the `ingest` bench target.
* Current source files under `src/`: `adapter.rs`, `batch.rs`, `config.rs`, `digest.rs`, `error.rs`, `explain.rs`, `lib.rs`, `main.rs`, `model.rs`, and `render.rs`.
* Public modules in `lib.rs`: `adapter`, `batch`, `config`, `digest`, `error`, `explain`, `model`, and `render`.
* CLI commands: `pipeline`, `batch`, `explain`, `config paths`, and `completions <shell>`.
* Current integration tests: `crates/aifix/tests/pipeline_cli.rs`.
* Current fixture: `crates/aifix/tests/fixtures/clippy.jsonl`.
* Current benchmark hook: `crates/aifix/benches/ingest.rs`, registering `clippy fixture parse and digest`.
* Current fuzz hook: `fuzz/fuzz_targets/ingest.rs`, registered by `fuzz/Cargo.toml` as target `ingest`.

Current integration tests in `pipeline_cli.rs` cover:

* clippy JSON pipeline output as JSON digest;
* TypeScript text pipeline output as markdown guidance;
* LSP JSON pipeline output as compact JSON digest;
* custom batch command execution through a real executable;
* bounded rejection of over-limit custom batch stdout;
* strict rejection of non-UTF-8 batch extra args;
* rejection of non-file project `aifix.toml` candidates.

Review hardening currently implemented:

* direct process execution without shell expansion;
* strict UTF-8 conversion for batch extra args;
* 1 MiB per-stream bounded batch capture;
* parseable nonzero diagnostic output can still render a digest;
* malformed structured-looking `auto` input is rejected at the structured boundary;
* TypeScript and LSP adapters validate blank fields and invalid or reversed ranges;
* digest dedupe excludes raw payload identity while preserving raw payloads for full JSON evidence.

## designed direction

The crate should stay a small diagnostic adapter, not a replacement for the underlying tools.
It should normalize tool output, preserve invocation metadata, group related diagnostics, and render the same digest shape for pipeline and batch modes.

Out of scope for this crate:

* network lookups while explaining diagnostic codes;
* applying fixes directly to source files;
* hiding nonzero tool exits when diagnostics are still parseable;
* maintaining compatibility aliases for old CLI or data-model names.

## open decision

* No measured ingestion timing baseline or CI threshold is recorded yet.
* No policy is recorded for when diagnostic suggestions are safe to apply.
* No committed shell completion artifacts are maintained; completions are generated on demand by `aifix completions <shell>`.
* The package name remains `aifix`; collision risk is documented as a naming concern, not a compatibility requirement.
