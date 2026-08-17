# Changelog

All notable changes to aifix are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/)

## [unreleased]

### Features

* Implement aifix diagnostic CLI
* _(mcp)_ Add diagnostic server
* _(cache)_ Add syntax-aware replay
* _(mcp)_ Harden fix replay cache
* _(config)_ Default to XDG config paths
* _(mcp)_ Advertise agent guidance
* _(agda)_ Support direct diagnostics
* _(agda)_ Support expected diagnostic gates
* _(cli)_ Add auto batch profile and profile discovery command
* _(mcp)_ Add aifix_batch_profiles and auto profile support
* _(batch)_ Add batch profile catalog and auto profile detection

### Bug Fixes

* _(agda)_ Ignore progress lines
* _(mcp)_ Import parse_diagnostics helper
* Return error for unexpected typescript state
* _(batch)_ Process large tool output with bounded spill files
* _(batch)_ Accept clean Clippy output and recover MCP transport after request deadlines

### Refactor

* Align Rust contracts with Wyrd
* _(mcp)_ Pass structured error content by reference
* _(batch)_ Extract built-in command families and inline helpers
* _(config)_ Dereference config command in run_config

### Documentation

* Add aifix project guidance
* Record optimization baseline
* Record MCP wrap-up state
* Strengthen bead wrap-up protocol
* Clarify incomplete bead workflow
* Design syntax cache matching
* Add project skill docs
* _(changelog)_ Document batch profile discovery and auto diagnostics
* Describe auto batch behavior and mcp profile guidance
* Refresh manifest hashes
* Refresh manifest after changelog format

### Testing

* Add isolated env helpers for cli and mcp integration tests
* Verify platform-native mode reports project dirs user path

### Continuous Integration

* Add release workflows
* Add report-only coverage workflow

### Miscellaneous Tasks

* Configure project tooling
* Add package metadata
* _(cargo)_ Allow publishing
* Add license metadata
* _(cargo)_ Add dist profile config
* _(release)_ Configure release tooling
* _(config)_ Update mise
* _(config)_ Update mise
* Ignore node_modules in gitignore and treefmt
* _(core)_ Adopt agentic-dev overlay
* _(ci)_ Adopt cached act parity

[unreleased]: https://github.com/silvanshade/aifix/compare/main...HEAD
