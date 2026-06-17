# Contributing

## Workflow

1. Start from the issue or bead that describes the change.
2. Read the relevant ADRs and project docs before editing.
3. Keep changes small, boring, and complete: update callers, tests, and docs together when behavior changes.
4. Prefer project-relative paths in docs, examples, and diagnostics.
5. Do not add compatibility shims, placeholder implementations, or mock behavior unless an accepted ADR explicitly requires them.

## Beads

Use beads for tracked work, blockers, and drift.
A bead that implements or changes an architectural decision should link the relevant ADR entry.
If a doc, manifest, or implementation detail drifts, create or update a bead instead of leaving an inline TODO.

## Gates

Before handing work off, run the narrow command that proves the behavior you changed.
Project-wide gates, tree formatting, markdown formatting, and release checks are run by the orchestrating workflow unless the task explicitly asks for them.

Never suppress a failing diagnostic to make a gate pass.
Fix the source or record the blocker in the bead.

## Documentation discipline

* Keep public docs publishable and concise.
* Update `CHANGELOG.md` for user-visible changes.
* Append new accepted decisions to `docs/ADR.md`; do not rewrite old decisions except to correct clerical errors.
* Keep `docs/MANIFEST.toml` aligned with tracked documentation artifacts.
* Do not include local machine paths, secrets, personal environment details, or unverifiable benchmark claims.
