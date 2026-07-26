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
User-level config discovery also needs to be predictable for agents across Linux, macOS, and Windows, while still allowing users to request platform-native directories explicitly.

### ADR-0003 decision

Discover optional user-level config first, then the nearest project `aifix.toml`.
Project config overrides user config.

Use XDG-style user config paths by default on every platform: if non-empty `XDG_CONFIG_HOME` is set, use `$XDG_CONFIG_HOME/aifix/aifix.toml`; otherwise, if non-empty `HOME` is set, use `$HOME/.config/aifix/aifix.toml`; otherwise, there is no user config path.
Platform-native config directories are available only as an explicit alternate mode through `AIFIX_CONFIG_DIR_MODE=platform-native` or `AIFIX_CONFIG_DIR_MODE=native`.
Any other `AIFIX_CONFIG_DIR_MODE` value is rejected as a typed configuration error.

Config may define default protocol, output format, maximum diagnostics, and profile command argv.

### ADR-0003 consequences

* Local preferences remain possible without leaking into the repository.
* Project config provides deterministic shared defaults.
* The default user config location is stable and XDG-style across operating systems.
* Users who prefer platform-native config directories can opt in without changing the default contract.
* Existing non-file config candidates and invalid config directory modes are rejected so malformed configuration state is visible instead of skipped.

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

## ADR-0008: Gate syntax-aware cache replay by confidence

Status: Accepted  
Date: 2026-06-20

### ADR-0008 context

The diagnostic fix cache can already replay previously reported patches by exact diagnostic signature.
That path is useful because it is deterministic: the cached diagnostic identity matches, the patch is checked by `git apply`, and application is explicit unless the caller asks for it.

Future reuse across nearby diagnostics is attractive, but approximate matching can apply a good-looking patch to the wrong code after edits, formatter changes, line-ending drift, parser failure, or diagnostic wording changes.
The project needs a design that records enough structure for later implementation without weakening the existing trusted exact-signature behavior.

### ADR-0008 decision

Keep exact diagnostic signatures as the only trusted unattended auto-apply path.
An exact-signature hit remains eligible for the existing replay behavior, including explicit apply requests after `git apply` validation.

Design any syntax-aware layer as a conservative suggestion layer before any model-driven generalization.
It must rank matches by confidence:

* `exact`: the exact diagnostic signature matches.
* `same-node`: the normalized diagnostic family and stable syntax node context match with high confidence.
* `nearby`: the diagnostic family matches and nearby syntax context, ancestors, siblings, tokens, or span deltas suggest a medium-confidence relation.
* `no-match`: no safe syntax-aware relation was found.

Only `exact` may preserve trusted auto-apply semantics.
`same-node` and `nearby` hits are suggestions and dry-runs only unless the caller explicitly requests application; explicit application still goes through `git apply --check` or an equivalent git validation before any patch is applied.
`no-match` produces no replay candidate.

Parser and language support must be pluggable and bounded.
Unknown languages, missing files, non-UTF-8 source, malformed spans, parser errors, excessive budget use, or unavailable parser dependencies degrade to exact-only behavior rather than guessing from raw diagnostic text.
Implementation dependency selection, including any tree-sitter or Rowan-style green/red tree crate choice, is deferred to a future implementation bead.

Use cache schema v2 for the syntax-aware design.
Schema v2 should store versioned match-index records and fix-family records rather than treating raw diagnostic payloads as cache identity.
The shape should include:

* normalized diagnostic family: source, code, severity, and message family;
* source file and recorded span/file identity;
* exact diagnostic signature for exact replay;
* syntax context fingerprint: stable node, ancestors, siblings, token context, and patch fingerprint/metadata;
* byte and line deltas plus whitespace and line-ending accounting;
* confidence floor and rank metadata;
* audit metadata explaining why a candidate was exact, same-node, nearby, or no-match.

Guardrails are part of the decision, not optional polish.
The matcher must not silently apply approximate hits, must not use raw diagnostic payload identity as the primary key, must not hide parser degradation, and must keep old v1 entries exact-only until enriched.
Audit output must report the selected confidence, degraded-parser or exact-only reasons, dry-run status, and git validation result.

Fixtures for the eventual implementation must cover exact migration, same-node and nearby suggestions, no-match cases, whitespace and line-ending drift, node movement, malformed spans, missing files, parser errors, no approximate auto-apply, dry-run audit output, and deterministic cache JSON.

### ADR-0008 consequences

* Existing exact-signature replay remains trusted and deterministic.
* Syntax-aware matching is accepted as a design, but implementation remains pending in filed beads.
* Approximate cache hits can help agents find likely fixes without granting them unattended patch authority.
* Parser failures and unsupported languages fail safe by falling back to exact-only matching.
* Cache schema evolution has a target shape, while concrete parser dependency choices remain deferred.

## ADR-0009: Accept opt-in model diagnostic generalization

Status: Accepted  
Date: 2026-06-20

### ADR-0009 context

ADR-0008 accepts exact-signature replay as the only trusted unattended path and syntax-aware matching as a bounded suggestion layer.
Some diagnostics may still fail exact and syntax-aware analytical matching after source movement, diagnostic wording changes, or related code edits.
The `aifix-iaz` design bead covers whether a model may help generalize from cache metadata in those remaining cases without weakening deterministic replay rules.

### ADR-0009 decision

Accept opt-in model diagnostic generalization as a design, not as implemented behavior.
Model use is opt-in only and is disabled unless a caller explicitly enables it for a request or configured workflow.
The model generalization layer may run only after exact replay and syntax-aware analytical matching do not produce a trusted match.

Model inputs must be bounded and structured.
They may include the normalized diagnostic family, source, code, severity, message family, cache match metadata, and a bounded syntax window around the diagnostic and candidate fix context.
They must not include arbitrary repository contents, unbounded source files, or unrelated cache entries.

Model outputs must use a structured contract that can represent at least `no-match`, candidate identity, confidence or rationale metadata, required user action, and failure reason.
Unparsable output, missing required fields, parser failure, source loading failure, model invocation failure, budget exhaustion, policy refusal, or source/model disagreement is an explicit `no-match` or fallback result.
These cases must not be converted into silent guesses.

No model-produced or model-generalized candidate may be applied unattended.
The result is advisory unless a caller explicitly requests application, and any application path still requires the same patch validation used by non-exact replay before modifying files.

Audit output is required.
The audit record must state that model generalization was opt-in, why exact and syntax-aware matching did not produce a trusted match, the diagnostic family and bounded source-window metadata used, the structured output status, any failure or fallback reason, dry-run status, user action required, and patch validation result when validation is attempted.

Cache schema metadata must distinguish model-generalized records from exact and syntax-aware records.
It must record model provider or local model identity, model version when available, prompt or contract version, input window bounds, diagnostic family, cache candidate identity, confidence metadata, creation time, and evaluation status.
This metadata must preserve v1 exact replay compatibility and schema v2 syntax-aware match-index and fix-family compatibility.

Implementation requires an evaluation and test plan before enabling the feature.
Fixtures must cover opt-in gating, exact-first behavior, syntax-aware-first behavior, bounded input construction, structured output parsing, explicit no-match and fallback cases, parser/source/model failures, no unattended auto-apply, audit records, cache metadata, deterministic disabled behavior, and regression cases where a plausible model suggestion must be rejected.

### ADR-0009 consequences

* Exact replay remains the only trusted unattended auto-apply path.
* Syntax-aware matching remains the analytical layer before any model use.
* Model generalization is design-accepted under `aifix-iaz`, but implementation remains pending.
* Parser, source, and model failures fail safe as explicit no-match or fallback outcomes.
* Future implementation work must include cache metadata, audit output, and evaluation fixtures before the feature can be enabled.

## ADR-0010: Harden MCP fix replay cache behavior

Status: Accepted  
Date: 2026-06-21

### ADR-0010 context

The wyrd pre-port verification found that MCP fix replay needed stronger behavior before high-volume Rust diagnostics relied on it.
The confirmed failures were source/code explanation collisions, replay aborting on missing target files, unclear `report_fix.signature` semantics, and a report-to-replay round-trip that could silently miss.
The same review raised hardening questions around noisy cargo streams, cache write concurrency, clean inputs, truncation visibility, path handling, and exact-line replay limits.

### ADR-0010 decision

Treat diagnostic explanations as a `(source, code)` classification.
Rustc and Clippy sources resolve to Rust explanations before any JavaScript or TypeScript lint fallback, and code-only JavaScript/TypeScript lint fallback is reserved for unknown or matching ecosystems.

Define `report_fix` identity explicitly.
When a diagnostic is supplied, the diagnostic-derived signature owns the cached patch; a mismatched explicit signature is rejected.
Signature-only reporting remains available for already validated exact cache keys.

Keep cache writes local but serialized.
Cache-mutating MCP paths use a project-local lock file around load, mutation, and atomic write-rename.
First cache initialization writes `.aifix/.gitignore` with `*` so consuming repositories do not accidentally commit tool-owned cache files.

Make replay failures per diagnostic.
Missing or unreadable Rust target files become stable audit fallback reasons, and mixed batches continue scoring other diagnostics.
Exact-signature matches remain trusted; syntax-aware matches remain suggestions or dry-run candidates unless explicit application passes the existing `git apply` validation.
Line-shifted exact patches are not guessed into place; failed context remains a replay miss or git validation failure.

Cargo JSONL parsing may retain valid compiler diagnostics beside isolated noisy or truncated lines, but malformed structured input with no valid diagnostics remains an error.
Empty input returns explicit zero-diagnostic results for the MCP surfaces that accept empty diagnostic sets.
`maxDiagnostics` caps retained/rendered samples only; aggregate counts and cache metrics still cover the full diagnostic set, and Markdown reports hidden samples.

### ADR-0010 consequences

* MCP agents can distinguish Rust/Clippy diagnostics from JavaScript lint-name collisions.
* Reported fixes either bind to the diagnostic-derived identity or fail before polluting the cache.
* Parallel cache writers are serialized without routing tool commands through a shell or adding dependencies.
* Missing generated or deleted files no longer abort replay for unrelated diagnostics.
* Exact-line patch churn remains an explicit limitation rather than an unsafe fuzzy apply.

## ADR-0011: Spill large batch streams behind bounded retention

Status: Accepted  
Date: 2026-07-18

### ADR-0011 context

The fixed 1 MiB per-stream cap from ADR-0006 rejected ordinary compiler runs with hundreds or thousands of diagnostics before parsing.
The Wyrd Rust profile used for `aifix-iy8` produced approximately 13.4 MB of cargo JSONL and nearly 2,000 normalized diagnostics.
Raising the in-memory cap would only move the failure and amplify raw output in invocation metadata.

### ADR-0011 decision

Supersede only ADR-0006's fixed-capture mechanism.
Keep separate 1 MiB stdout and stderr prefixes in invocation metadata, but spill larger complete streams to private temporary files for parsing.
Validate complete streams incrementally as UTF-8 and parse cargo compiler-message JSONL one record at a time.
Expose complete stream byte counts even when invocation bodies contain only prefixes.

Retain a hard, configurable per-stream processing budget for runaway tools.
The default is 1 GiB; CLI `--max-output-bytes`, MCP `maxOutputBytes`, and root or profile `max_output_bytes` configuration may override it.
Continue to execute direct argv without a shell and preserve parseable nonzero diagnostic results.

### ADR-0011 consequences

* Ordinary multi-megabyte compiler output no longer fails at the invocation-retention boundary.
* Raw invocation evidence remains bounded in memory while parsers receive complete input.
* Spill files are create-new, mode `0600` on Unix, and deleted when capture state leaves scope.
* Large diagnostic-count memory remains proportional to normalized diagnostics and their preserved raw records, not the complete child stream.
* Explicit output budgets still fail with a stream-, executable-, and byte-specific process error.

## ADR-0012: Run explicit native fixes before residual diagnostics

Status: Accepted  
Date: 2026-07-25  
Bead: `aifix-tip.1`

### ADR-0012 context

Batch mode previously captured diagnostics without mutating source.
An agent that wanted tool-native automatic fixes had to invoke the tool separately, reconstruct profile-specific argv, then call `aifix` again to learn which diagnostics remained.
That duplicated adapter knowledge and made the residual-diagnostic contract unavailable through the MCP batch tool.

Native fix support varies by profile and can mutate a dirty workspace.
Treating a generic `--fix` flag as text to append to every diagnostic command would invent unsupported tool behavior and could place flags on the wrong side of an argv separator.

### ADR-0012 decision

Add an explicit opt-in native-fix phase to batch execution.
CLI `--fix` and MCP `fix: true` request mutation; diagnostic-only behavior remains the default.

Each profile owns an optional native fix argv.
The built-in Rust profile uses `cargo clippy --fix --allow-dirty` with the same target, feature, message-format, and lint-cap settings as its diagnostic pass.
Configured profiles may declare `fix_argv`; commands remain direct argv and never pass through a shell.
Profile discovery reports native-fix capability.

A native fix runs once before the ordinary diagnostic command.
Successful fix commands may emit non-diagnostic output.
A nonzero fix exit is tolerated only when the effective fix-output protocol—`fix_protocol` when configured, otherwise the diagnostic protocol—is explicit, non-automatic, and parses at least one diagnostic, because automatic generic-text fallback could misclassify an operational error; other nonzero output is an operational failure.
The returned digest is built only from the subsequent diagnostic pass.

For a named profile, requesting a missing native fix command is an explicit argument error with configuration recovery.
For `auto`, fixable detected profiles run their native fix phase while profiles without one still run diagnostically and report that limitation in profile status metadata.

### ADR-0012 consequences

* Agents can request one mutating batch operation and receive only residual diagnostics.
* Profile configuration is flexible enough for other native fix tools through complete direct argv and an optional fix-output protocol, without exposing arbitrary shell commands.
* Rust `--allow-dirty` fixes intentionally permit unstaged and staged changes; Cargo's missing-VCS safeguard remains active.
* Native fix output uses the existing bounded capture and UTF-8 contracts.
* LSP code actions remain a separate capability because their lifecycle and safety policy are not expressible as one profile argv.

## ADR-0013: Apply bounded diagnostic-correlated LSP code actions

Status: Accepted  
Date: 2026-07-25  
Bead: `aifix-tip.2`

### ADR-0013 context

Some language servers expose precise repairs that compiler and linter fix modes do not.
The Language Server Protocol returns code actions for a document range and diagnostic context; actions may contain workspace edits, commands, deferred resolution data, or interactive choices.
Applying every returned action would be unsafe because actions can be ambiguous, disabled, stale, unrelated to the requested diagnostic, or backed by commands with arbitrary server-defined behavior.

### ADR-0013 decision

Add a one-shot, opt-in LSP code-action phase to batch execution.
CLI `--code-actions` and MCP `codeActions: true` request this mutation; ordinary batch and pipeline runs remain diagnostic-only.
When native fix and code-action modes are both requested, native fixes run first, LSP actions run against the resulting workspace, and the ordinary profile diagnostic command runs last.
Every requested mutation capability is validated before the first phase changes the workspace, including all participating profiles in automatic mode.

Each profile may own nested `code_actions` configuration: complete direct server argv, language ID, source extensions, allowed hierarchical action kinds, exact allowed command identifiers, iteration cap, and request timeout.
The built-in Rust profile defaults to `rust-analyzer`, Rust source files, `quickfix`, no command allowlist, 64 iterations, and 30 seconds.
Named profiles without this capability fail explicitly.
Automatic runs mutate only when exactly one detected profile supports code actions, reject multiple capable mutators before any mutation, and run unsupported profiles diagnostically.

The client initializes one direct-argv stdio LSP process, opens matching non-symlink source files under the canonical workspace root, and collects pushed diagnostics after a bounded idle interval.
For each diagnostic it requests `textDocument/codeAction` with that diagnostic and the configured action kinds.
Deferred actions may use `codeAction/resolve`.
Disabled, uncorrelated, out-of-kind, payloadless, edit-plus-command, and unallowlisted-command actions are rejected.
A diagnostic is changed only when it has one eligible action or exactly one eligible preferred action; competing actions remain residual.

Workspace edits may change text in already opened files only.
Before applying an edit, the target must still match the last content synchronized with the server, remain under the canonical root, and satisfy any document version.
UTF-16 ranges must be valid and non-overlapping; mixed workspace-edit representations, resource operations, confirmation-required change annotations, and actions combining edits with commands are rejected.
All files changed by one workspace edit are staged beside their targets before replacement begins.
On Linux and macOS, each replacement atomically exchanges the staged and target inodes, validates the displaced target against synchronized content and metadata, and removes it only after validation.
A detected concurrent save is exchanged back; if safe restoration cannot be proved, the displaced file is retained and its path is reported.
Any later replacement failure attempts to restore every prior file before returning a typed error.
Platforms without an atomic file-exchange primitive reject code-action mutation during preflight.
Server commands run only when their exact configured identifiers also appear in the server's advertised command capability.
Each command may submit at most one `workspace/applyEdit` request through the ordinary validation and rollback path; out-of-scope, repeated, malformed, or unsafe requests receive `applied: false`.
The exact command allowlist is a profile trust boundary: aifix cannot mediate filesystem mutations, process launches, or other effects that the language-server process performs without `workspace/applyEdit`.
After each mutation the client synchronizes changed documents according to the server-selected full or incremental mode, waits for bounded diagnostic quiescence, and requests the next action.
Versioned diagnostic publications older than the opened document cannot replace current state; current unversioned and unopened-document publications remain visible as residuals.
Repeated action keys, unchanged successful actions, iteration exhaustion, the complete-session deadline, bounded request and notification queues, blocked or oversized messages, malformed protocol data, server failure, and stale edits are typed errors.
Transient LSP `ContentModified` responses receive at most three retries inside the original request deadline.

Residual LSP diagnostics are normalized through the existing adapter and combined with diagnostics from the final profile invocation.
Process execution remains shell-free; source discovery, aggregate action queries, messages, pending notifications, complete session time, and server stderr retention are bounded.

### ADR-0013 consequences

* Agents can request rust-analyzer and configured-server quick fixes in the same one-shot batch interface as native fixes.
* Automatic mutation is intentionally conservative: ambiguous or interactive actions remain for an agent or user.
* A granular approval mode is feasible, but it needs a separate preview/selection interface with stable action summaries and workspace-version tokens so selections cannot be replayed against stale source; this decision does not add that protocol.
* Push diagnostics have no universal completion notification for a clean document, so the client uses bounded message quiescence and the final native diagnostic invocation remains the authoritative residual compiler or linter pass.
