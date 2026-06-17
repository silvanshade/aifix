# Architecture Decision Log

This file is append-only for accepted project decisions.
New decisions are added as new numbered entries.
Older entries may be superseded by later entries, but should not be rewritten except for clerical corrections.

## ADR-0001: Build a Rust CLI workspace

Status: Accepted  
Date: 2026-06-16

### ADR-0001 context

`aifix` needs to run locally inside developer and agent workflows, consume tool output from stdin or files, and optionally invoke tools directly.
The initial implementation must avoid network dependencies and keep process boundaries simple.

### ADR-0001 decision

Implement `aifix` as a Rust 2024 CLI in a workspace package at `crates/aifix`, with both a binary and library named `aifix`.

### ADR-0001 consequences

* Rust gives a small native CLI with predictable process execution.
* The library/CLI split keeps future non-CLI integrations possible.
* Workspace gates must verify the crate rather than only documentation.

## ADR-0002: Use a normalized diagnostic core over direct SARIF

Status: Accepted  
Date: 2026-06-16

### ADR-0002 context

Supported tools emit different shapes: compiler-message JSON lines, TypeScript text, LSP diagnostics, and generic text lines.
SARIF can represent full static analysis runs, but that breadth is larger than the repair-oriented digest agents need.

### ADR-0002 decision

Normalize every input into the project `Diagnostic` model before digesting or rendering.
Treat SARIF and LSP as adapter formats, not as the internal model.

### ADR-0002 consequences

* Digest logic is independent of any one protocol.
* Renderers can stay stable while adapters evolve.
* SARIF support can be added later without replacing the core model.

## ADR-0003: Layer user and project configuration

Status: Accepted  
Date: 2026-06-16

### ADR-0003 context

Individual users may want defaults, while repositories need project-specific commands and output choices.
The CLI should work without configuration and avoid requiring a repository-specific directory structure.

### ADR-0003 decision

Discover optional user-level config first, then the nearest project `aifix.toml`.
Project config overrides user config.
Config may define default protocol, output format, maximum diagnostics, and profile command argv.

### ADR-0003 consequences

* Local preferences remain possible without leaking into the repository.
* Project config provides deterministic shared defaults.
* Existing non-file config candidates are rejected so malformed project state is visible instead of skipped.

## ADR-0004: Explain codes locally and deterministically

Status: Accepted  
Date: 2026-06-16

### ADR-0004 context

Agents need quick context for error and lint codes, but live network lookups make CLI output slower, less reproducible, and harder to test.

### ADR-0004 decision

Implement `aifix explain` as a deterministic local mapper.
It returns stable references, status, and short summaries for known rustc, clippy, TypeScript, oxlint-like, and unknown code forms.

### ADR-0004 consequences

* Explanations are reproducible and safe for offline workflows.
* The command provides hints, not authoritative fetched documentation.
* Adding new code families is a data/model change, not a network feature.

## ADR-0005: Link beads and ADRs for documented drift

Status: Accepted  
Date: 2026-06-16

### ADR-0005 context

The project uses beads for tracked work and ADRs for durable decisions.
Drift in code, docs, or manifests should be visible instead of hidden in comments or stale documentation.

### ADR-0005 decision

Use beads to track implementation work, blockers, and drift.
Beads that implement or revise architectural decisions should reference the relevant ADR entry.
ADRs record accepted decisions; beads carry execution state.

### ADR-0005 consequences

* Decision history remains stable and concise.
* Work tracking can change without rewriting accepted decisions.
* Manifest drift should become an explicit bead when not fixed in the same change.

## ADR-0006: Harden diagnostic boundaries during review

Status: Accepted  
Date: 2026-06-17

### ADR-0006 context

The initial CLI implementation added the required pipeline, batch, explain, configuration, completion, rendering, test, bench, and fuzz surfaces.
Review then identified boundaries that must fail explicitly rather than accepting surprising or resource-heavy inputs.

### ADR-0006 decision

Harden the runtime boundaries as follows:

* Batch commands are executed as direct argv vectors, never through a shell.
* Batch extra arguments after `--` must be strict UTF-8.
* Batch stdout and stderr capture is capped at 1 MiB per stream before UTF-8 conversion or invocation retention.
* `auto` protocol rejects malformed structured-looking cargo JSON, complete JSON, LSP, or native `aifix` payloads instead of falling back to generic text.
* TypeScript and LSP adapters reject blank required fields; LSP also rejects malformed or reversed ranges.
* Digest deduplication excludes preserved raw payload identity and uses normalized semantic fields.

### ADR-0006 consequences

* Agent-facing output is less likely to hide malformed structured diagnostics.
* Batch mode remains predictable under large or invalid child output.
* Raw payloads can remain available for evidence without making duplicate detection allocate around raw JSON identity.

## ADR-0007: Generate shell completions from clap metadata

Status: Accepted  
Date: 2026-06-17

### ADR-0007 context

The CLI already uses clap for argument definitions.
Agents and humans benefit from completions that match the current command surface without maintaining separate shell scripts.

### ADR-0007 decision

Expose `aifix completions <shell>` using clap-complete and the generated clap command metadata.

### ADR-0007 consequences

* Completion output tracks CLI enum and argument definitions.
* Supported shells are the shells supported by clap-complete.
* The repository does not maintain committed generated completion scripts yet.
