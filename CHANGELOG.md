# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and is compatible with git-cliff sectioning.

## [Unreleased]

### Added

* Initial documentation set for the `aifix` Rust CLI.
* Agent-first diagnostic adapter design covering pipeline mode, batch mode, normalized diagnostics, digest rendering, local explanations, configuration inspection, and shell completion generation.
* Documentation manifest for tracked project docs.
* Local Rust crate-selection skill adapted for `aifix` dependency decisions.
* Rust workspace crate `crates/aifix` with CLI commands `pipeline`, `batch`, `explain`, `config paths`, and `completions <shell>`.
* MCP stdio server exposing diagnostic pipeline, batch, dedupe, fix-cache replay, fix reporting, and learned guidance tools.
* ADR-0008 documents the conservative syntax-aware fix-cache matching design.
* Syntax-aware diagnostic fix-cache schema v2, Rust syntax matching, replay audit metadata, and safety coverage for conservative exact/same-node/nearby/no-match replay.

### Changed

* Replaced initial implementation brief in `AGENTS.md` with post-project-init maintainer guidance for future agents.
* Documented review hardening: bounded 1 MiB batch stream capture, strict non-UTF-8 batch argument rejection, structured-input rejection in `auto`, TypeScript and LSP adapter validation, direct argv execution, and digest deduplication that excludes raw payload identity.
* Clarified that batch mode can return a digest for parseable diagnostics from a nonzero tool exit while still failing on unparsable nonzero output.
* Clarified docs discipline around ADRs, beads, and manifest hash refresh after documentation changes.
* Strengthened bead-scoped wrap-up guidance to require transactional commits, clean working trees, and explicit close/amend/split bead state.
* Clarified incomplete bead workflow: epic-shaped remaining work, roadmap beads, transactional status updates, residual task beads, and prompt-final summaries.
* Accepted ADR-0009 for opt-in model diagnostic generalization after exact and syntax-aware cache matching fail.

### Notes

* The project name `aifix` is tentative due to collision risk.
  Suggested alternatives remain `diagflow`, `lintrelay`, `fixroute`, and `signalfix`.

[Unreleased]: https://github.com/silvanshade/aifix/compare/HEAD...HEAD
