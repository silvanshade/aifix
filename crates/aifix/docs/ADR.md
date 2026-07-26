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

### Run explicit native fixes before residual diagnostics

Decision: opt-in batch native-fix mode runs a profile-owned direct argv once, then builds the returned digest from a fresh diagnostic invocation.

Rationale: tool-native fixers know which suggestions are machine-applicable.
Keeping fix argv in the profile avoids generic flag injection while a post-fix diagnostic pass gives agents the residual findings they need.

Consequences:

* The Rust built-in uses `cargo clippy --fix --allow-dirty`.
* Configured profiles may provide a complete `fix_argv` independent of diagnostic extra args.
* Configured profiles may override the protocol used to classify nonzero fix output; the effective protocol must be non-automatic and parse at least one diagnostic.
* Named unsupported profiles fail; automatic runs continue diagnostically for unsupported profiles.
* Native-fix output uses the same bounded process capture and UTF-8 contracts.

### Apply diagnostic-correlated LSP actions conservatively

Decision: opt-in batch code-action mode runs a profile-owned direct-argv language server, applies only deterministic diagnostic-correlated actions within bounded safety constraints, then combines published residuals with a fresh tool diagnostic pass.

Rationale: language servers can expose precise quick fixes that compiler-native fix modes omit, but returned edits and commands are not inherently safe to apply unattended.
Keeping server lifecycle, correlation, edit validation, and convergence behind one module preserves a small batch interface.

Consequences:

* The Rust built-in defaults to `rust-analyzer`; configured profiles own server argv, source matching, action kinds, exact command allowlists, iteration caps, and timeouts.
* A sole eligible action or sole preferred action may apply; competing, disabled, uncorrelated, interactive, or unallowlisted actions remain residual.
* Text edits are restricted to opened in-root files whose contents still match synchronized state, with matching optional versions and valid non-overlapping UTF-16 ranges; mixed edit representations, resource operations, confirmation-required annotations, and edit-plus-command actions are rejected.
* An allowlisted command must also appear in the server's advertised command capability.
  It executes only while one `workspace/applyEdit` request may enter the same edit-validation and rollback path; all out-of-scope, repeated, malformed, or unsafe edit requests receive `applied: false`.
* Exact command identifiers are a profile trust boundary: aifix cannot mediate filesystem mutations or other effects that the language-server process performs without `workspace/applyEdit`.
* Multi-file workspace edits validate and stage completely before replacement, preserve source-owned ACLs and extended attributes, and attempt rollback if replacement fails.
  Linux and macOS replacements atomically exchange staged and target inodes, validate the displaced target, restore detected concurrent saves, and retain a displaced file when restoration cannot be proved.
  Other platforms reject code-action mutation during preflight. macOS may add only its system-managed `com.apple.provenance` attribute to a staged file; current unversioned and unopened-document diagnostics remain residual; complete session time, action queries, pending notifications, messages, and server stderr are bounded.
* Automatic mode permits at most one detected code-action-capable profile so a later mutator cannot stale an earlier server's residual diagnostic snapshot; other detected profiles still run diagnostically.
* Every requested mutation capability is preflighted before mutation; native fixes precede code actions when both are requested, and the ordinary diagnostic invocation remains last.
* A future granular approval interface is feasible only with action previews plus workspace-version tokens; it is not part of one-shot mode.

## designed direction

### Treat batch execution as diagnostic capture

Decision: batch mode should preserve stdout, stderr, command argv, cwd, and exit code in invocation metadata while still producing a digest when a nonzero tool exit yields parseable diagnostics.

Rationale: linters and compilers often use nonzero exits to report findings.
Agents need both the parsed diagnostic shape and the original invocation facts to understand whether execution failed operationally or reported code issues.

Consequences:

* Renderers should not discard invocation metadata.
* Unparsable nonzero output should remain an operational failure.

### Keep diagnostic suggestions explicit

Decision: suggestions should be represented only when an input adapter observes them; the crate should not invent edits from normalized diagnostic text.

Rationale: diagnostic formats differ in whether suggestions are precise, machine-applicable, or only explanatory.
Native-fix mode delegates applicability to the owning tool rather than applying parsed suggestions itself.

Consequences:

* The digest can guide an agent toward likely next files and codes.
* Applying parsed suggestions remains outside the crate until a separate accepted decision defines safety rules.

## open decision

* No size, latency, or diagnostic-count budgets have been accepted as gates.
* No measured benchmark threshold is recorded.
* No policy defines when a parsed suggestion is machine-applicable enough to use automatically.
