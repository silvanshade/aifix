# aifix

`aifix` is an agent-first Rust adapter for turning noisy tool diagnostics into a small, structured digest that an LLM coding agent can act on.
Pipeline mode and ordinary batch runs do not apply fixes.
Batch native-fix mode and MCP cached replay apply mode mutate source only when explicitly requested.
It normalizes diagnostics, deduplicates exact semantic repeats, groups related issues, preserves tool invocation metadata, and renders the result for agent handoff.

The project name is tentative.
`aifix` has collision risk, including an existing PyPI name.
Possible alternatives are `diagflow`, `lintrelay`, `fixroute`, and `signalfix`; they are suggestions only until the project is renamed or published.

## Build prerequisites

Compiling `aifix` on Linux requires the development package for POSIX ACLs.
On Debian and Ubuntu, install it with `apt-get install libacl1-dev`.
The ACL support preserves source access controls during transactional LSP workspace edits.

## Modes

### Pipeline mode

Pipeline mode reads diagnostics that another command already produced, then emits the common digest.

```sh
aifix pipeline --protocol clippy-json --format markdown --input clippy.jsonl
cargo clippy --message-format=json | aifix pipeline --protocol clippy-json --input -
```

Use pipeline mode when an editor, CI step, or local script already owns tool execution.

### Batch mode

Batch mode runs `auto`, a built-in profile, or a configured profile directly, captures stdout, stderr, and exit status, then parses any supported diagnostics into the same digest.

```sh
aifix batch --format compact-json
aifix batch auto --format markdown
aifix batch rust --format compact-json -- -W clippy::pedantic
aifix batch rust --fix --format compact-json
aifix batch rust --code-actions --format compact-json
aifix batch agda --protocol agda-text -- -i src src/Main.agda
aifix batch custom --protocol nushell-text -- nu-lint scripts/check.nu
```

With no profile, batch mode defaults to `auto`; `aifix batch auto` selects the same profile explicitly.
`auto` detects applicable Rust, TypeScript, Agda, and Nushell project shapes, runs detected or defaultable profiles, aggregates parseable diagnostics, and reports per-profile `profile_statuses`.
An operational failure in one detected profile is reported for that profile without discarding parseable diagnostics from other profiles.
Built-in profiles target Rust, TypeScript, Agda, and Nushell.
Custom profiles require an explicit command argv.
Commands are executed without a shell.
Extra arguments after `--` are profile-specific, must be valid UTF-8, and append to named diagnostic commands plus built-in fallback fix commands; explicit configured `fix_argv` is complete and independent, and `auto` rejects extra arguments.
Each stream retains at most 1 MiB in invocation metadata; larger output spills to private temporary storage for complete parsing, with a configurable 1 GiB default per-stream processing budget.
Override the budget with `--max-output-bytes`, root or profile `max_output_bytes` config, or MCP `maxOutputBytes`.
A nonzero tool exit can still produce a digest when diagnostics are parseable; unparsable nonzero output remains an `aifix` process error.
`--fix` runs a profile-declared native fix command once, then reruns the ordinary diagnostic command and renders only diagnostics that remain.
The built-in Rust profile uses `cargo clippy --fix --allow-dirty`, intentionally permitting unstaged and staged changes while retaining Cargo's missing-VCS safeguard.
Configured profiles can provide direct-argv `fix_argv`.
Named profiles without a fix command fail explicitly; `auto --fix` fixes supported profiles and runs unsupported profiles diagnostically.
`--code-actions` opens a profile-owned language server, requests diagnostic-correlated actions, applies only a unique configured automatic action, then runs the ordinary diagnostic command and combines both residual sources.
When both mutation phases are requested, aifix validates every requested capability before changing files, runs the native fixer first, then runs LSP actions against the updated workspace.
The Rust built-in defaults to `rust-analyzer`; configured profiles provide a complete server `argv`, language ID, source extensions, allowed action kinds, exact command allowlist, iteration cap, and timeout under `[profiles.<name>.code_actions]`.
Direct edits and exact allowlisted action commands are eligible.
An allowlisted command must be advertised by the server and may submit at most one `workspace/applyEdit` request; unsolicited, repeated, malformed, or unsafe requests receive `applied: false`.
Workspace edits are limited to synchronized opened files inside the selected root, require current document versions when supplied, reject resource operations or confirmation-required annotations, and preserve source-owned ACLs and extended attributes.
After all changed files validate and stage, Linux and macOS replacements use atomic exchange: aifix validates the displaced target inode, restores it if a concurrent save won the race, and retains the displaced file when safe restoration cannot be proved.
Platforms without atomic file exchange reject code-action mutation during preflight.
The profile author trusts each allowlisted command not to mutate files or launch side effects outside `workspace/applyEdit`; aifix cannot mediate hidden effects performed directly by the language-server process.
On macOS, the operating system may add its managed `com.apple.provenance` attribute to a staged replacement; no other staging-only attribute is accepted.
Named profiles without code-action support fail explicitly.
`auto --code-actions` mutates only when exactly one detected profile supports code actions, rejects multiple capable mutators before any mutation, and runs unsupported profiles diagnostically.

### MCP mode

MCP mode exposes the same diagnostic core over newline-delimited stdio JSON-RPC for Claude Code and other Model Context Protocol clients:

```sh
aifix mcp
```

The server advertises tools for pipeline and batch digests, batch profile discovery, diagnostic dedupe, cached-fix reporting, cached-fix replay, and diagnostic-shape guidance; its initialize response also includes concise agent tool guidance.
`aifix_batch_profiles` lists `auto`, built-ins, `custom`, and configured profiles with detection metadata.
`aifix_batch` accepts an omitted or empty `profile` as `auto`, and unknown profiles return structured recovery data for choosing a valid profile.
Set MCP `aifix_batch` argument `fix` to `true` only when the caller intends to mutate the workspace before receiving residual diagnostics.
Set MCP `aifix_batch` argument `codeActions` to `true` only when the caller intends to apply bounded, diagnostic-correlated LSP actions before receiving residual diagnostics.
Set MCP `aifix_batch` argument `timeoutMs` on calls without workspace mutations or cache updates that need a client-compatible deadline.
Expiry returns a structured `request-timeout`; later batch calls return `batch-in-progress` until that diagnostic process finishes, while other JSON-RPC requests remain available.
Project-local cache state is stored in `.aifix/diagnostics.json`.
Cached fix replay feeds stored patches to `git apply` through direct argv and stdin; `suggest` mode returns patch text without invoking Git.

#### Agent tool guidance

* Use `aifix_pipeline` when another tool already produced diagnostic output.
* Use `aifix_batch_profiles` when unsure which batch profiles exist; do not guess profile names such as `cargo-check`.
* Prefer `aifix_batch` with omitted, empty, or `auto` profile for project-wide diagnostics.
* Use a named batch profile when the run is intentionally profile-specific.
* Pass batch extra arguments only to named profiles; `auto` rejects them because they are profile-specific.
* Set batch `fix` to `true` only for an explicit mutating run; the result contains diagnostics from the post-fix pass.
* Set batch `codeActions` to `true` only for an explicit mutating LSP run; unsafe, ambiguous, disabled, stale, or unallowlisted actions remain unapplied.
* Set batch `timeoutMs` when the client needs a bounded wait; after a structured timeout, wait for the active diagnostic process to finish before retrying with a larger deadline.
* Use `aifix_dedupe` and `aifix_guidance` for repeated project-local diagnostic triage and handoff guidance.
* Use `aifix_report_fix` and `aifix_replay_fixes` only for explicitly recorded cached patch replay; respect `suggest`, `dry-run`, and `apply` modes.
* `aifix` normalizes diagnostics and does not invent fixes.
* Parseable diagnostics from nonzero tool exits are findings, not an operational failure.
* Agents should still verify repairs with the native tools and tests that own the code.

## Protocols and output formats

Current input protocols:

* `auto`
* `aifix-json`
* `clippy-json` for rustc/clippy compiler-message JSON lines
* `typescript-text` for `tsc --pretty false` style diagnostics
* `agda-text` for direct Agda CLI diagnostics
* `lsp-json` for diagnostic arrays or `publishDiagnostics` params
* `nushell-text` for generic non-empty diagnostic lines

`auto` rejects malformed structured-looking input instead of silently converting it to generic text.
TypeScript and LSP adapters reject blank required fields and invalid or reversed ranges at the adapter boundary; the Agda adapter groups direct CLI headers with multiline bodies.

Current output formats:

* `json`: full digest, including preserved raw payloads when adapters have them
* `compact-json`: digest without raw fields and with sample diagnostics only
* `markdown`: grouped agent guidance

Digest deduplication uses normalized semantic fields only; preserved raw payloads are excluded from duplicate identity.

## Other commands

```sh
aifix mcp
aifix explain rustc E0308
aifix explain clippy clippy::needless_borrow
aifix config paths
aifix config profiles --format json
aifix completions bash
```

`mcp` runs the agent-facing stdio server.
`explain` is deterministic and local.
It returns stable references and short summaries instead of performing network lookups.
`config profiles` lists discoverable batch profiles in `json`, `compact-json`, or `markdown`; the listing includes `auto`, built-ins, `custom`, and configured profiles with detection metadata.
`completions` writes a shell completion script for any shell supported by clap-complete.

## Configuration

`aifix` loads optional user configuration first and the nearest project `aifix.toml` second.
Project configuration overrides user defaults.
By default, user configuration uses the same XDG-style path policy on every platform: `$XDG_CONFIG_HOME/aifix/aifix.toml` when `XDG_CONFIG_HOME` is non-empty, otherwise `$HOME/.config/aifix/aifix.toml` when `HOME` is non-empty.
If neither variable is available, there is no user config path.
Set `AIFIX_CONFIG_DIR_MODE=platform-native` or `AIFIX_CONFIG_DIR_MODE=native` to opt in to platform-native user config directories.
Any other `AIFIX_CONFIG_DIR_MODE` value is a configuration error.
Config may set the default protocol, output format, maximum diagnostics, per-stream output-byte budget, named profile diagnostic `argv`, optional native-fix `fix_argv`, and an optional `fix_protocol`; nonzero fix output is accepted only when the effective protocol is non-automatic and parses at least one diagnostic.
Configured profiles appear in `config profiles` and MCP `aifix_batch_profiles` output alongside `auto`, built-ins, and `custom`; discovery metadata reports native-fix support.
Existing non-file `aifix.toml` candidates are rejected so broken project state is visible.
