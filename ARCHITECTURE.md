# Architecture

`aifix` is a diagnostic adapter, not a replacement for linters, compilers, LSP servers, or SARIF producers.
Its center is a small normalized diagnostic model that is easier for coding agents to consume than raw tool output.

The project name remains tentative until publication.

## Core flow

```text
tool output -> adapter -> Diagnostic[] -> digest -> renderer -> agent handoff
                    batch runner --------^          ^
                    config discovery ---------------|
```

## Normalized diagnostic core

The internal model preserves the fields agents need to plan repairs:

* source protocol and tool name
* severity
* file span when known
* stable code when supplied by the tool
* message and optional suggestion text
* invocation metadata for batch runs

The model is intentionally smaller than SARIF and more stable than any one LSP payload.
Adapters absorb tool-specific details so digesting, grouping, and rendering stay boring.

## CLI and MCP surfaces

The binary uses clap and clap-complete for human-invoked commands.
Current CLI commands are:

* `pipeline`: parse existing stdin or file diagnostics.
* `batch`: run a configured or built-in profile, then parse captured output.
* `mcp`: run the newline-delimited stdio MCP server.
* `explain`: return deterministic local metadata for diagnostic code families.
* `config paths`: print considered user and project config paths.
* `completions <shell>`: write a shell completion script.

The MCP server is the primary agent surface.
It advertises tools for pipeline digests, batch digests, project-local diagnostic dedupe, cached-fix reporting, cached-fix replay, and diagnostic-shape guidance.
Tool failures are returned as MCP tool results so one bad diagnostic payload does not terminate the stdio session.
Cached fix replay uses direct `git apply` argv with patch text on stdin; no shell mediates patch checks or application.

## Adapters

Adapters parse supported protocols into normalized diagnostics:

* rustc/clippy compiler-message JSON lines
* TypeScript text diagnostics from `tsc --pretty false`
* LSP diagnostic arrays and `textDocument/publishDiagnostics` params
* native `aifix` JSON
* Nushell or generic line-oriented text

`auto` mode probes structured inputs first.
Malformed complete JSON, LSP, or native-looking diagnostic payloads are rejected at their structured boundary instead of falling through to generic text.
Cargo JSONL streams skip non-diagnostic events and may retain valid compiler diagnostics beside isolated noisy or truncated lines; malformed cargo-shaped input with no valid diagnostics remains an error.
TypeScript parsing rejects blank required fields, and LSP parsing rejects blank messages plus malformed or reversed ranges.

A failed tool exit is not automatically a failed `aifix` run.
If diagnostics can be parsed, batch mode returns a digest and records the nonzero exit code in the invocation.
Nonzero output that cannot be parsed remains a process error.

## Digest

The digest layer deduplicates exact semantic repeats, counts by source and severity, groups by source plus code, and falls back to source plus message when a code is absent.
Each group carries representative samples capped by the configured maximum and renderers report when additional samples are hidden.

Deduplication excludes preserved raw JSON payload identity.
Raw payloads remain available in full JSON output, but duplicate identity is based on normalized source, code, severity, message, spans, and suggestions.

This keeps the output useful for agents without hiding that multiple files or severities are involved.

## Project-local diagnostic cache

MCP cache tools persist deterministic JSON at `.aifix/diagnostics.json` under the selected project root.
First cache initialization also writes `.aifix/.gitignore` with `*` so consuming repositories do not accidentally commit tool-owned cache files.
Cache mutations use a project-local lock file around load, mutation, and atomic write-rename so parallel agents do not corrupt the JSON file or lose updates.
The cache stores schema-versioned diagnostic signatures, already-surfaced diagnostics, cached patch text, and diagnostic-shape metrics.
Stable signatures are computed from normalized source, code, severity, message, spans, and suggestions.
They exclude preserved raw JSON payloads.

The dedupe tool records emitted signatures and suppresses repeats on later calls; this is a persistent per-project seen set, not a regression detector across clean sessions.
The fix-cache tools let an agent record a successful patch for a diagnostic signature, then later request suggestions, dry-run checks, or direct application when that signature recurs.
Replay returns per-diagnostic audit entries for missing or unreadable target source files instead of aborting mixed batches.
Exact-signature replay remains the only unattended apply path; line-shifted patches still rely on `git apply` context and can be reported as misses rather than guessed.
The guidance tool aggregates source, severity, code, and signature counts into deterministic Markdown for per-project agent guidance.

## Renderers

* Full JSON preserves the digest exactly.
* Compact JSON omits raw diagnostic payloads and captured stdout/stderr bodies, retaining invocation metadata and byte counts.
* Markdown renders grouped guidance for agents.

## Batch runner

Batch mode invokes one configured profile and captures stdout, stderr, exit code, working directory, and argv.
Built-in defaults are deliberately conventional:

```text
rust:
  cargo clippy --quiet --message-format=json --all-targets --all-features --
  --cap-lints warn
typescript: tsc --noEmit --pretty false
nushell: nu-lint
custom: explicit argv only
```

The runner owns process execution through `std::process::Command`; commands are never routed through a shell.
Extra args after the CLI `--` separator are accepted only when they are strict UTF-8.
Stdout and stderr are captured separately with a 1 MiB per-stream limit before UTF-8 conversion or invocation retention.

## Configuration layering

Configuration discovery reads optional user-level config and then the nearest project `aifix.toml`.
Project config overrides user config.
The supported surface is intentionally small: default protocol, default output format, maximum diagnostics, and profile command argv.

Existing non-file config candidates are rejected instead of skipped.
This lets teams standardize local behavior without making the CLI depend on any one repository layout or hiding malformed project state.

## SARIF and LSP boundary

SARIF and LSP are important interchange formats, but they are not the internal model:

* SARIF is broad enough to encode entire analysis runs, rules, taxonomies, and result provenance.
  `aifix` needs a compact repair-oriented view.
* LSP diagnostics are editor transport payloads.
  They omit some batch invocation context and vary by server.
* Agents benefit from stable grouping, count summaries, and capability notes that are not native to either format.

Therefore SARIF and LSP can be input or output adapters.
The core stays the normalized diagnostic model so future formats do not force a rewrite of digest logic.
