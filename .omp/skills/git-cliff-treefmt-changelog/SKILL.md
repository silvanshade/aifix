---
name: git-cliff-treefmt-changelog
description: Use when wiring git-cliff changelog generation into a treefmt/rumdl-managed repository with optional docs-manifest hashes.
---

# Git-cliff with treefmt/rumdl

Use this when adding or repairing git-cliff-generated `CHANGELOG.md` in a repository where Markdown is formatted or linted by treefmt/rumdl and documentation files may be content-hash registered.

## Procedure

1. Inspect the repository's existing changelog, formatter, task-runner, and docs-manifest conventions before choosing template shape.
2. Add `git-cliff` to the pinned toolchain and expose two tasks:
   + generate: `git-cliff --output CHANGELOG.md`
   + check: generate to a temporary file and compare to `CHANGELOG.md` when the installed git-cliff lacks `--check`.
3. Make the `cliff.toml` template formatter-stable before relying on check tasks:
   + Use the bullet marker emitted by the project's Markdown formatter.
   + Avoid reference-style headings such as `[Unreleased]` unless the template also emits matching footer references.
   + Trim template whitespace so generated output has no extra blank lines that the formatter will remove.
4. Generate `CHANGELOG.md`, run the formatter, then diff a fresh temporary `git-cliff` output against the formatted file.
5. If the commit that adds release docs should appear in the generated changelog:
   + commit once,
   + run changelog generation again,
   + run the formatter,
   + refresh any docs-manifest hash for `CHANGELOG.md`,
   + amend the just-created commit.
6. Verify with the changelog check, treefmt check, docs-manifest check when present, and the project gates relevant to metadata changes.

## Failure modes

* `treefmt --ci` changes `CHANGELOG.md`: the template is not formatter-stable.
* `rumdl` reports MD052 for `[Unreleased]`: add footer links or use plain text headings.
* Changelog check fails only after committing: the new commit belongs in the generated changelog; regenerate and amend.
