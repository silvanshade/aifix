# Knowledge Discipline

`aifix` keeps project knowledge explicit, reviewable, and cheap for agents to load.
The rule is simple: durable context belongs in tracked docs, while current work and drift belong in beads.

## Content-addressed docs manifest

Tracked docs are listed in `docs/MANIFEST.toml` with a content hash field.
During initialization placeholder hashes are acceptable; maintained branches should let the formatter or manifest update step refresh them.

A changed doc with a stale manifest is drift.
Fix the manifest when the content change is intentional.
Open or update a bead when the drift exposes unresolved work.

After documentation-only edits, refresh manifest hashes with the project formatter/manifest workflow before publishing or handing off a final branch.
Do not hand-edit hashes unless the manifest workflow is unavailable and the fallback is explicitly documented in the bead or handoff note.

## Typed edge vocabulary

Use stable relationship words when linking project knowledge:

* `decides`: an ADR establishes a rule or direction.
* `implements`: a bead or change applies an ADR.
* `supersedes`: a later ADR replaces an earlier decision.
* `depends-on`: work requires another decision or artifact first.
* `documents`: a file records behavior that exists in code.
* `drifts-from`: an artifact no longer matches the source it claims to describe.

Prefer these terms in beads, ADR notes, and manifest comments when relationships matter.

## Drift produces beads

Do not bury drift in TODO comments.
If code, docs, ADRs, or manifests disagree and the fix is not part of the current change, create or update a bead that names the mismatch and links the affected artifacts.

When a bead implements, revises, or follows from an architectural decision, link it to the relevant ADR number.
ADRs hold accepted decisions; beads hold execution state, blockers, and follow-up drift.

## Diagnostic cache scope status

Exact-signature diagnostic replay is implemented through the MCP surface and
the project-local `.aifix/diagnostics.json` cache.
This covers stable diagnostic signatures, cached patch reporting, replay
suggestions, dry-run checks, direct `git apply` application, dedupe state, and
deterministic diagnostic-shape guidance.

Model-driven generalization is not implemented.
Approximate syntax-aware matching, confidence-scored cache key families, and
opt-in small-model repair generalization remain tracked work under bead
`aifix-a02` until implemented or split into narrower beads.

## Project vs. contributor concerns

Project knowledge is durable and shared: architecture, accepted decisions, workflow rules, public examples, and manifest entries.
Put it in tracked docs.

Contributor knowledge is local and temporary: shell aliases, editor setup, machine paths, private credentials, and personal notes.
Keep it out of tracked project docs.
