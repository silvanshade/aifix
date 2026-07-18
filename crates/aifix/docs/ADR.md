# Architecture Decisions

## current

### Normalize before rendering

Decision: diagnostics are normalized into crate-owned model types before digest construction or rendering.

Rationale: pipeline mode and batch mode need the same downstream artifact even when their inputs arrive from different tools.
A small normalized model lets the adapter deduplicate, count, group, and sample diagnostics without coupling the renderer to each tool's wire format.

Consequences:

* `model.rs` owns protocols, output formats, severities, spans, suggestions, diagnostics, invocations, counts, groups, and digests.
* `adapter.rs` is the only layer that understands source-specific protocol details.
* `render.rs` consumes the digest model rather than reparsing raw tool payloads.

### Keep explanation deterministic and local

Decision: `explain::explain_code` returns deterministic metadata for known code families without network access.

Rationale: an agent-facing diagnostic adapter must be stable in offline and CI contexts.
Explanation hints should identify likely reference families, not fetch mutable external content or claim to know a tool's full semantics.

Consequences:

* Rust `E####`, Clippy lint paths, TypeScript `TS####`, oxlint-like codes, and unknown forms receive stable local explanations.
* Markdown and JSON renderers can attach explanation metadata without network variability.

### Merge project configuration over user configuration

Decision: configuration discovery reads optional user configuration and nearest project `aifix.toml`, with project fields overriding user fields.

Rationale: personal defaults are useful, but a repository needs deterministic crate-local behavior for diagnostic profiles, output format, default protocol, and maximum diagnostics.

Consequences:

* `config::Config` models default protocol, output format, maximum diagnostics, and named profile argv.
* Existing non-file config candidates are rejected instead of silently skipped.
* `aifix config paths` exposes considered user and project paths.

### Invoke tools directly, not through a shell

Decision: batch execution builds argv vectors and invokes them with `std::process::Command`.

Rationale: diagnostic capture should preserve the exact command boundary and not inherit shell quoting or expansion behavior.

Consequences:

* Built-in Rust, TypeScript, and Nushell commands are represented as argv lists.
* Custom batch profiles require an explicit executable as the first argv item.
* Extra args are appended as argv entries after strict UTF-8 conversion.

### Bound retained batch output while processing complete diagnostics

Decision: stdout and stderr keep separate 1 MiB prefixes in memory.
Larger streams spill to private temporary files, remain parseable up to a configurable per-stream budget that defaults to 1 GiB, and expose their complete byte counts in invocation metadata.

Rationale: a fixed 1 MiB capture cap rejected ordinary compiler runs with hundreds or thousands of diagnostics.
Retaining every raw byte in memory would instead make tool output an unbounded memory input.
Spilling separates the parser's complete-input requirement from the agent-facing invocation evidence budget.

Consequences:

* Stdout and stderr remain separate, and only their prefixes are retained in the digest.
* Spill files use create-new names, mode `0600` on Unix, and scope-bound deletion.
* Complete streams are validated as UTF-8 incrementally before parsing.
* Cargo compiler-message JSONL and text protocols are parsed one record at a time; complete JSON decodes directly from readers.
* `max_output_bytes` and `--max-output-bytes` retain a hard storage budget for runaway tools.
* Over-budget streams return process errors naming the stream, executable, and selected budget.

### Reject malformed structured input in auto mode

Decision: `Protocol::Auto` rejects structured-looking cargo JSON, complete JSON, LSP, or native `aifix` payloads when the matched structured parser rejects them.

Rationale: falling back to generic text for malformed structured diagnostics would hide adapter errors and produce misleading agent guidance.

Consequences:

* Malformed structured diagnostics fail at the protocol boundary.
* Unstructured text can still fall back through TypeScript parsing and then generic/nushell line diagnostics.

### Deduplicate without raw payload identity

Decision: digest duplicate identity is based on normalized semantic fields and excludes preserved raw JSON payloads.

Rationale: raw source payloads can be large and source-specific.
They are useful as evidence but should not decide whether two normalized diagnostics are the same repair signal.

Consequences:

* Full JSON can preserve raw payload evidence.
* Compact JSON omits raw payloads.
* Deduplication avoids serializing raw payloads into identity keys.

### Keep library errors typed

Decision: library failures use the crate-owned `AifixError` enum instead of a generic error wrapper.

Rationale: callers and the CLI need to distinguish IO, JSON, TOML, config, process, parser, UTF-8, and argument failures without parsing strings.

Consequences:

* `error.rs` owns the public error type and result alias.
* CLI errors wrap library errors rather than erasing their source category.

### Generate completions from clap metadata

Decision: `aifix completions <shell>` uses clap-complete against the same clap command definition used for runtime parsing.

Rationale: generated completion scripts should track the actual command surface without maintaining hand-written shell files.

Consequences:

* Completion output is generated on demand.
* Supported shells are defined by clap-complete.
* No generated completion artifacts are committed by the crate-local docs.

## designed direction

### Treat batch execution as diagnostic capture

Decision: batch mode should preserve stdout, stderr, command argv, cwd, and exit code in invocation metadata while still producing a digest when a nonzero tool exit yields parseable diagnostics.

Rationale: linters and compilers often use nonzero exits to report findings.
Agents need both the parsed diagnostic shape and the original invocation facts to understand whether execution failed operationally or reported code issues.

Consequences:

* Renderers should not discard invocation metadata.
* Unparsable nonzero output should remain an operational failure.

### Keep fix capability explicit

Decision: suggestions and fixes should be represented only when an input adapter observes them; the crate should not invent edits.

Rationale: diagnostic formats differ in whether suggestions are precise, machine-applicable, or only explanatory.
Invented fixes are unsafe for an agent-first adapter.

Consequences:

* The digest can guide an agent toward likely next files and codes.
* Applying edits remains outside this crate until a separate accepted decision defines safety rules.

## open decision

* No size, latency, or diagnostic-count budgets have been accepted as gates.
* No measured benchmark threshold is recorded.
* No policy defines when a parsed suggestion is machine-applicable enough to use automatically.
