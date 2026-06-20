# Agent guidance

`aifix` is a Rust workspace for an agent-first diagnostic adapter.
The CLI turns noisy compiler, linter, LSP, and text diagnostics into a normalized digest that coding agents can consume.
It does not apply fixes.

The project name is still tentative until publication; keep the collision caveat in public docs.

## Workspace commands

Use the narrowest command that proves your change.

* Format: `cargo fmt --all`
* Build: `cargo build --workspace --all-targets`
* Lint: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
* Test: `cargo nextest run --workspace`
* CLI smoke examples:
  + `aifix pipeline --protocol clippy-json --format markdown --input crates/aifix/tests/fixtures/clippy.jsonl`
  + `aifix explain rustc E0308`
  + `aifix completions bash`

Do not suppress warnings, skip tests, or narrow a verification claim beyond what you actually ran.

## Rust design policy

* Keep the crate boring and explicit: adapters parse, the model normalizes, digest groups and deduplicates, renderers render, and the CLI dispatches.
* Prefer clean cutovers.
  Do not leave aliases, compatibility shims, or deprecated call paths unless an accepted ADR requires them.
* Public and private Rust items should carry useful rustdoc explaining contract, failure modes, and panic behavior.
* Design by contract: document preconditions, postconditions, recoverable errors, and panic expectations on nontrivial functions.
* Panic policy: production paths return typed errors.
  Panics are unacceptable except for debug assertions of internal invariants or test-only failures.
* Preserve direct argv process execution in batch mode.
  Never route tool commands through a shell.
* Treat nonzero tool exits with parseable diagnostics as diagnostic results, not automatic `aifix` failures.
* Reject invalid boundaries explicitly: malformed structured auto input must not fall back to generic text, non-UTF-8 batch extra args must fail, and batch capture stays bounded.
* Do not allocate or copy unless it makes ownership or output necessary.
  Avoid serializing raw payloads for identity or dedupe.

## Dependency policy

Before adding or changing Rust dependencies, use the local skill at `.omp/skills/find-best-rust-crates`.
Dependency choices must consider maintenance health, feature footprint, transitive size, security response, and contributor reputation.
Prefer standard library and existing workspace dependencies when they are enough.

## Documentation, ADRs, and knowledge discipline

* Root docs describe project behavior; crate-local docs under `crates/aifix/docs/` describe crate implementation details.
* Record accepted architectural decisions in `docs/ADR.md`; crate-local decisions may also be mirrored in `crates/aifix/docs/ADR.md` when they affect crate maintenance.
* Keep docs factual.
  Do not describe placeholders, scaffolds, or planned work as implemented behavior.
* Update changelogs when user-visible or maintainer-visible behavior changes.
* After doc edits, refresh `docs/MANIFEST.toml` hashes with the project formatter/manifest workflow before final publication.
* Track ongoing work and drift in beads.
  Beads that implement or revise decisions should link the relevant ADR entry; ADRs record decisions, beads record execution state.

## Wrap-up protocol

When finishing bead-scoped work, leave durable state instead of a loose working tree.

* Commit each completed slice with a focused, coherent commit; include the required agent `Co-Authored-By` trailer on agent-created commits.
* Finish with `git status --short` clean, except for explicitly identified user-owned changes or authorized uncommitted work.
* Update affected docs, changelog entries, and `docs/MANIFEST.toml` hashes before publishing the final commit when the slice changes them.
* Close a bead only when its full recorded scope is complete and verification is noted.
* If scope remains, amend the bead with observed verification, unresolved work, and links to durable docs or ADRs.
* Split follow-up beads before closing parent work so remaining scope stays tracked.
* When partial work remains, make the remaining state epic-shaped: promote the current bead to an epic or attach it beneath a newly surfaced epic.
* Add or update a roadmap bead under that epic that captures the intended sequence and acceptance boundaries.
* After each transactional task, update the roadmap bead and any touched subtask beads with current status, verification notes, dependencies, and child/subtask state.
* File residual task beads under the epic as they surface from subtasks; do not leave known follow-up work only in prose.
* In the final prompt response, summarize current bead state, what changed, verification performed or skipped, and any new beads filed.
* Push bead state with `bd dolt push` when the bead database has a configured remote.
* Do not commit only when per-task user guidance forbids it, or when committing or recording durable state would be dangerous/destructive and the user has not authorized the override.

## Verification expectations

Every nontrivial change needs proof.
Run the tests or smoke scenario that exercises the changed behavior, plus any directly affected unit or integration tests.
If you cannot run a gate, state exactly why and what narrower evidence you obtained.

When updating docs only, do not run formatters or gates unless explicitly requested; review the rendered content and keep it aligned with observed code and prior verification.
