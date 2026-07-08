# Changelog

All notable changes to aifix are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/)

## [unreleased]

### Features

* Implement aifix diagnostic CLI
* _(mcp)_ Add diagnostic server
* _(mcp)_ Advertise agent tool guidance during initialize
* _(cache)_ Add syntax-aware replay
* _(mcp)_ Harden fix replay cache
* _(config)_ Default to XDG config paths
* _(agda)_ Add direct CLI diagnostic support
* _(config)_ List discoverable batch profiles with JSON, compact JSON, and Markdown output
* _(batch)_ Default omitted batch profiles to `auto` and aggregate applicable Rust, TypeScript, Agda, and Nushell profile diagnostics
* _(batch)_ Guide extra args as profile-specific and reject extra args for `auto`
* _(mcp)_ Expose batch profile listing, omitted or empty batch profiles, and structured unknown-profile recovery data

### Fixed

* _(agda)_ Ignore `Checking …` and `Finished …` progress lines during Agda text parsing and auto protocol detection

### Refactor

* Align Rust contracts with Wyrd

### Documentation

* Add aifix project guidance
* Record optimization baseline
* Record MCP wrap-up state
* Strengthen bead wrap-up protocol
* Clarify incomplete bead workflow
* Design syntax cache matching
* Add project skill docs

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

[unreleased]: https://github.com/silvanshade/aifix/compare/main...HEAD
