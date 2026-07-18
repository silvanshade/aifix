# Optimization

Crate scope: `crates/aifix`.

This document records performance and optimization knowledge for the current aifix documentation baseline.
It is source-grounded in `METRICS.md`, `ADR.md`, `STATUS.md`, and `TODO.md`; it does not record Rust/API behavior changes.

Status vocabulary in this file is limited to `current`, `designed direction`, and `open decision`.

## current

* Benchmark coverage is one Criterion bench source: `benches/ingest.rs` with target `ingest`.
* The measured baseline is the 2026-06-17 local Criterion run recorded in `docs/METRICS.md`.
  + Observed command: `cargo bench -p aifix --bench ingest`.
  + Observed row: `clippy fixture parse and digest`.
  + Observed estimate: `[6.2256 µs 6.2491 µs 6.2730 µs]`.
* This row measures the current in-process hot path for a small embedded clippy compiler-message JSONL fixture:
  + parse clippy JSONL through `adapter::parse_diagnostics(Protocol::ClippyJson, ...)`;
  + build an agent digest through `digest::build_digest(..., Some(16))`;
  + exclude process startup, subprocess execution, filesystem reads, stdout rendering, and terminal IO.
* The benchmark fixture is intentionally small.
  It is useful as a smoke-level trend anchor, not as a representative maximum-throughput workload.
* The current benchmark does not measure:
  + batch process execution or output spilling;
  + shell completion generation;
  + config discovery;
  + markdown, JSON, or compact JSON rendering;
  + LSP, TypeScript text, normalized aifix JSON, or generic/nushell text parsing;
  + allocation counts;
  + digest output byte size;
  + behavior at large diagnostic counts;
  + fuzz corpus throughput.
* `current`: Batch retains at most 1 MiB from each stream in memory, spills larger complete streams, and enforces a configurable per-stream processing budget that defaults to 1 GiB.
  Line-oriented cargo and text protocols retain one input record at a time in addition to normalized diagnostics; complete JSON decodes directly from readers without an intermediate full-input string.
* One-shot debug-build evidence collected on 2026-07-18, not a release benchmark baseline:
  + a Wyrd Rust profile produced 13.3–13.4 MB of cargo JSONL and 1,956–1,957 deduplicated diagnostics; the former implementation aborted at 1 MiB, while spilled batch processing completed in 1.01 seconds with an 84.1 MB peak footprint reported by `/usr/bin/time -l`;
  + 100,000 synthetic compiler-message records totaling 18.3 MB completed in 1.00 second with a 246.5 MB peak footprint; this shows that large-count memory is dominated by retained normalized diagnostics, not raw stream size.
* `current`: Digest duplicate detection fingerprints normalized semantic fields and intentionally excludes raw payload identity.
  The optimization value is avoiding raw JSON amplification during dedupe; no allocation-count baseline has been measured.
* `current`: `auto` protocol probing rejects malformed structured-looking inputs rather than falling back to generic text.
  That correctness choice may cost a small amount of extra detection work; no separate benchmark exists for `auto` probing.
* `current`: The CLI is expected to outperform the exemplar Nushell scripts on the in-process parsing/digest path because it avoids shell pipelines, repeated process calls, dynamic Nushell table transformations, and JSON reshaping in an interpreter.
  This is an engineering expectation from implementation shape, not a measured head-to-head comparison.

## designed direction

* Refresh release-build baselines only through orchestrator-owned benchmark runs.
* Track adapter paths separately:
  + clippy/rustc compiler-message JSONL;
  + TypeScript text diagnostics;
  + LSP JSON diagnostics;
  + normalized aifix JSON;
  + generic/nushell text lines;
  + `auto` protocol probing.
* Track pipeline stages separately:
  + parse only;
  + digest construction only;
  + render only;
  + parse plus digest;
  + parse plus digest plus render.
* Add larger fixture curves before optimizing internals:
  + 1 diagnostic;
  + 10 diagnostics;
  + 100 diagnostics;
  + 1,000 diagnostics;
  + mixed duplicate diagnostics;
  + large raw payloads that should not affect dedupe identity.
* Track output-size metrics separately from latency:
  + full JSON bytes;
  + compact JSON bytes;
  + markdown bytes;
  + retained invocation stdout/stderr bytes;
  + group count and sample count.
* Add allocation-count baselines before optimizing clone or collection behavior.
* Keep batch-process benchmarks separate from parser benchmarks because process spawn, child output volume, and OS pipe behavior dominate different costs.
* Keep `auto` protocol correctness tests paired with any `auto` optimization, because the security boundary is rejecting malformed structured input rather than accepting the fastest generic fallback.
* Treat the 1 MiB retention threshold and configurable total-output budget as policy inputs.
  Change either only with representative throughput and peak-memory evidence.
* If binary protocol support is added later, benchmark it beside JSON adapters with equivalent diagnostic content rather than replacing the JSON baseline.

## open decision

* Regression threshold for the `ingest` benchmark row.
* Hardware-normalized benchmark policy.
* Allocation-count baseline tool and reporting format.
* Large-fixture shape and whether fixtures should be generated or checked in.
* Maximum supported diagnostic count for an agent-facing digest.
* Maximum rendered digest byte budget for compact JSON and markdown.
* Whether batch process execution deserves a benchmark despite OS noise.
* Whether `auto` protocol probing should have a separate budget.
* Whether head-to-head comparison against exemplar Nushell scripts is worth maintaining, and if so which scripts and fixture corpus define a fair comparison.
