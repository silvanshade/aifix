---
name: reference-project-convention-drift
description: "Use when checking or aligning a Rust project's conventions against a reference project, including DbC docs, property tests, lint posture, and work tracking."
---

# Reference Project Convention Drift Check

Use when asked to align this project's Rust or workflow conventions with a reference project, or when drift between the two may matter.

## Procedure

1. Identify the current project root and the reference project root from the request, repo context, or prior memory.
   In this repository, the usual comparison is local project conventions against Wyrd.
2. Read the reference project's current convention source first, especially workflow/coding-convention docs and representative Rust files/tests.
   Treat memory as a hint, not current truth.
3. Map the local surface with targeted searches, not file-by-file browsing:
   + Rust `# Contract` blocks and stale clause labels.
   + Test/bench lint relaxations.
   + Cargo dependency feature posture.
   + Existing property-test coverage.
   + Formatter, changelog, and docs-manifest workflows.
4. Track substantial alignment work in the project's durable tracker.
   Prefer an epic with child tasks for contract docs, property tests, and other convention parity when the work spans sessions or many files.
5. Delegate parallel slices when the change spans many files.
   Subagents edit only; the orchestrator runs formatters and gates once over the combined change.
6. For Wyrd-style design-by-contract rustdoc, use clauses in this order: `- requires:`, `- ensures:`, `- provides:`, `- fails:`, `- panics:`.
   Keep `# Errors` for `Result` functions.
   Remove low-value contract blocks on constants, trivial accessors, thin wrappers, and obvious data holders.
7. Before adding Rust dependencies, use the project's dependency-selection guidance.
   For `proptest`, Wyrd treats `default-features = true` as an explicit exception because the test runtime depends on defaults; keep it dev-only unless production strategies are required.
8. Verify with the current project's documented gates.
   For this Rust workspace, the usual set is:
   + `cargo fmt --all`
   + targeted tests for new/changed behavior
   + `cargo build --workspace --all-targets`
   + `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   + `cargo nextest run --workspace`
   + formatter/docs-manifest checks when docs changed
   + old-label search plus a small contract-label/order validator when DbC docs changed
9. Update changelog and docs-manifest artifacts when user- or maintainer-visible docs or behavior changed.
10. If the repo workflow expects commits, commit a clean coherent slice with the required trailer, close/update durable tracker items with verification notes, push tracker state when configured, and report tool caveats.

## Known caveats

* If a project exists to support a reference project, periodically suggest checking convention drift against that reference.
* Avoid broad reference-project lint-posture imports unless specifically scoped; they can create unrelated churn.
