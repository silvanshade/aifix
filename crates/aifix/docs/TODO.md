# TODO

## current

* Keep crate-local docs aligned with observable `crates/aifix` source, tests, benchmarks, and fuzz hooks as the CLI evolves.
* Keep `aifix` focused on diagnostic ingestion, digest construction, local explanation, batch invocation capture, typed errors, rendering, configuration, and shell completion generation.
* Preserve root `AGENTS.md` as repository guidance; crate docs should add crate-local facts rather than replace repository policy.
* Treat current review hardening as implemented behavior: direct argv execution, strict UTF-8 extra args, bounded per-stream capture, structured auto rejection, adapter field validation, and raw-excluding digest dedupe.

## designed direction

* Keep pipeline and batch modes rendering the same digest shape.
* Expand adapter coverage only with fixture-backed cases for supported protocols: normalized aifix JSON, clippy/rustc compiler-message JSONL, Agda direct CLI text, TypeScript text, LSP diagnostic arrays and publishDiagnostics params, and nushell or generic text lines.
* Keep batch profile defaults boring and explicit for Rust, TypeScript, Agda, Nushell, and custom commands.
* Keep remaining Agda project policy, such as multi-root orchestration or repository-wide `--without-K` sweeps, outside the generic diagnostic adapter unless a future feature explicitly adds that scope.
* Expand benchmark coverage from the observed ingest baseline before optimizing parser, digest, render, or batch capture internals.
* Grow fuzz coverage from the current `ingest` target by preserving minimized crashing inputs as regression fixtures.
* Decide whether generated shell completion scripts should become release artifacts or remain on-demand output.
* Document package-name collision risk for `aifix` only as a naming concern; suggested alternatives remain diagflow, lintrelay, fixroute, and signalfix.

## open decision

* No accepted latency, output-size, or diagnostic-count budgets exist yet.
* No fuzz corpus policy or runtime target exists yet.
* No policy exists for when suggestions can be considered machine-applicable.
