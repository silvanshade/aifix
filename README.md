# aifix

`aifix` is an agent-first Rust CLI for turning noisy tool diagnostics into a small, structured digest that an LLM coding agent can act on.
It does not apply fixes.
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

Batch mode runs a configured profile directly, captures stdout, stderr, and exit status, then parses any supported diagnostics into the same digest.

```sh
aifix batch rust --format compact-json
aifix batch typescript --protocol typescript-text --cwd .
aifix batch custom --protocol nushell-text -- nu-lint scripts/check.nu
```

Built-in profiles target Rust, TypeScript, and Nushell.
Custom profiles require an explicit command argv.
Commands are executed without a shell.
Extra arguments after `--` must be valid UTF-8, and each captured stream is bounded to 1 MiB.
A nonzero tool exit can still produce a digest when diagnostics are parseable; unparsable nonzero output remains an `aifix` process error.

## Protocols and output formats

Current input protocols:

* `auto`
* `aifix-json`
* `clippy-json` for rustc/clippy compiler-message JSON lines
* `typescript-text` for `tsc --pretty false` style diagnostics
* `lsp-json` for diagnostic arrays or `publishDiagnostics` params
* `nushell-text` for generic non-empty diagnostic lines

`auto` rejects malformed structured-looking input instead of silently converting it to generic text.
TypeScript and LSP adapters reject blank required fields and invalid or reversed ranges at the adapter boundary.

Current output formats:

* `json`: full digest, including preserved raw payloads when adapters have them
* `compact-json`: digest without raw fields and with sample diagnostics only
* `markdown`: grouped agent guidance

Digest deduplication uses normalized semantic fields only; preserved raw payloads are excluded from duplicate identity.

## Other commands

```sh
aifix explain rustc E0308
aifix explain clippy clippy::needless_borrow
aifix config paths
aifix completions bash
```

`explain` is deterministic and local.
It returns stable references and short summaries instead of performing network lookups.
`completions` writes a shell completion script for any shell supported by clap-complete.

## Configuration

`aifix` loads optional user configuration first and the nearest project `aifix.toml` second.
Project configuration overrides user defaults.
Config may set the default protocol, output format, maximum diagnostics, and named profile commands.
Existing non-file `aifix.toml` candidates are rejected so broken project state is visible.
