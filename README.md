# aifix

`aifix` is an agent-first Rust adapter for turning noisy tool diagnostics into a small, structured digest that an LLM coding agent can act on.
CLI digest modes do not apply fixes.
The MCP surface can replay explicitly recorded project-local patches when an agent requests that rerere-style behavior.
It normalizes diagnostics, deduplicates exact semantic repeats, groups related issues, preserves tool invocation metadata, and renders the result for agent handoff.

The project name is tentative.
`aifix` has collision risk, including an existing PyPI name.
Possible alternatives are `diagflow`, `lintrelay`, `fixroute`, and `signalfix`; they are suggestions only until the project is renamed or published.

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
aifix batch rust --format compact-json -- --all-targets
aifix batch agda --protocol agda-text -- -i src src/Main.agda
aifix batch custom --protocol nushell-text -- nu-lint scripts/check.nu
```

With no profile, batch mode defaults to `auto`; `aifix batch auto` selects the same profile explicitly.
`auto` detects applicable Rust, TypeScript, Agda, and Nushell project shapes, runs detected or defaultable profiles, aggregates parseable diagnostics, and reports per-profile `profile_statuses`.
An operational failure in one detected profile is reported for that profile without discarding parseable diagnostics from other profiles.
Built-in profiles target Rust, TypeScript, Agda, and Nushell.
Custom profiles require an explicit command argv.
Commands are executed without a shell.
Extra arguments after `--` are profile-specific, must be valid UTF-8, and are rejected by `auto`.
Each captured stream is bounded to 1 MiB.
A nonzero tool exit can still produce a digest when diagnostics are parseable; unparsable nonzero output remains an `aifix` process error.

### MCP mode

MCP mode exposes the same diagnostic core over newline-delimited stdio JSON-RPC for Claude Code and other Model Context Protocol clients:

```sh
aifix mcp
```

The server advertises tools for pipeline and batch digests, batch profile discovery, diagnostic dedupe, cached-fix reporting, cached-fix replay, and diagnostic-shape guidance; its initialize response also includes concise agent tool guidance.
`aifix_batch_profiles` lists `auto`, built-ins, `custom`, and configured profiles with detection metadata.
`aifix_batch` accepts an omitted or empty `profile` as `auto`, and unknown profiles return structured recovery data for choosing a valid profile.
Project-local cache state is stored in `.aifix/diagnostics.json`.
Cached fix replay feeds stored patches to `git apply` through direct argv and stdin; `suggest` mode returns patch text without invoking Git.

#### Agent tool guidance

* Use `aifix_pipeline` when another tool already produced diagnostic output.
* Use `aifix_batch_profiles` when unsure which batch profiles exist; do not guess profile names such as `cargo-check`.
* Prefer `aifix_batch` with omitted, empty, or `auto` profile for project-wide diagnostics.
* Use a named batch profile when the run is intentionally profile-specific.
* Pass batch extra arguments only to named profiles; `auto` rejects them because they are profile-specific.
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
Config may set the default protocol, output format, maximum diagnostics, and named profile commands.
Configured profiles appear in `config profiles` and MCP `aifix_batch_profiles` output alongside `auto`, built-ins, and `custom`.
Existing non-file `aifix.toml` candidates are rejected so broken project state is visible.
