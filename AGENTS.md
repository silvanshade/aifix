# Agent Guidance

Compact aifix project delta for coding agents — keep shared doctrine in the core, and keep this file focused on project-specific constraints.

`aifix` is a Rust workspace for an agent-first diagnostic adapter.
The CLI turns noisy compiler, linter, LSP, and text diagnostics into a normalized digest that coding agents can consume.
Mutation is restricted to explicit modes; ordinary pipeline and batch runs do not apply fixes.
The project name is still tentative until publication; keep the collision caveat in public docs.

## Shared core

The shared operating doctrine arrives with the gandr-conventions conformance rework (tracked on silvanshade/vault-gandr#101); until it lands, the gandr-lang/gandr AGENTS chain is the reference.


## Project delta

* **Orientation**: adapters parse diagnostics, the model normalizes, digest groups and deduplicates, renderers render, and the CLI dispatches.
  Keep the crate boring and explicit.
* **Diagnostic contract**: `aifix` reports normalized diagnostics and never invents fixes.
  It applies changes only through explicit mutating modes accepted in the ADR; ordinary runs remain diagnostic-only.
  Treat nonzero tool exits with parseable diagnostics as diagnostic results, not automatic `aifix` failures.
* **CLI boundary handling**: malformed structured auto input must not fall back to generic text; non-UTF-8 batch extra args must fail; batch capture stays bounded.
* **Batch execution**: preserve direct argv process execution in batch mode.
  Never route tool commands through a shell.
  A nonzero native-fix exit requires at least one diagnostic parsed by an explicit non-automatic protocol; signal termination is always an operational failure.
* **LSP mutation safety**: code-action mode is explicit, diagnostic-correlated, bounded, and profile-configured.
  Never broaden automatic selection beyond deterministic allowed kinds, apply edits outside opened in-root documents, or execute server commands without an exact allowlist.
* **Rust panic policy**: production paths return typed errors.
  Panics are unacceptable except for debug assertions of internal invariants or test-only failures.
* **Rust contracts**: public and private nontrivial Rust items should carry useful rustdoc explaining contract, failure modes, and panic behavior.
  Document preconditions, postconditions, recoverable errors, and panic expectations; keep `# Errors` for `Result` functions.
* **Cutovers**: prefer clean cutovers.
  Do not leave aliases, compatibility shims, or deprecated call paths unless an accepted ADR requires them.
* **Efficiency**: do not allocate or copy unless it makes ownership or output necessary.
  Avoid serializing raw payloads for identity or dedupe.
* **Dependencies**: before adding or changing Rust dependencies, use the local crate-selection skill.
  Consider maintenance health, feature footprint, transitive size, security response, and contributor reputation; prefer the standard library and existing workspace dependencies when they are enough.
* **Docs and ADRs**: root docs describe project behavior; crate-local docs under `crates/aifix/docs/` describe crate implementation details.
  Record accepted architectural decisions in `docs/ADR.md`; mirror crate-local decisions in `crates/aifix/docs/ADR.md` when they affect crate maintenance.
* **Doc truthfulness**: keep docs factual.
  Do not describe placeholders, scaffolds, or planned work as implemented behavior.
  The project name remains tentative until publication.
* **Manifest discipline**: when publishing doc edits that affect registered docs, refresh `docs/MANIFEST.toml` hashes with the project formatter/manifest workflow.
  For docs-only agent assignments that explicitly ban formatters or gates, review rendered content instead and report the skipped manifest refresh.
* **Tracking**: track ongoing work and drift in beads per the core workflow.
  Beads that implement or revise decisions should link the relevant ADR entry.

## Commands and gates

Use the narrowest command that proves your change:

* Format: `cargo fmt --all`
* Build: `cargo build --workspace --all-targets`
* Lint: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
* Test: `cargo nextest run --workspace`
* Docs/manifest: use the treefmt/docs-manifest workflow where registered docs changed.
* CLI smoke examples:
  + `aifix pipeline --protocol clippy-json --format markdown --input crates/aifix/tests/fixtures/clippy.jsonl`
  + `aifix explain rustc E0308`
  + `aifix completions bash`

Do not suppress warnings, skip tests, or narrow a verification claim beyond what you actually ran.
For now, ignore linter failures whose diagnostic target is `./CHANGELOG.md` in this local aifix repo; do not ignore unrelated failures.

Every nontrivial change needs proof: run the tests or smoke scenario that exercises the changed behavior, plus any directly affected unit or integration tests.
For docs-only changes, do not run formatters or gates unless explicitly requested; review the rendered content and keep it aligned with observed code and prior verification.
