---
name: find-best-rust-crates
description: Research, compare, and select Rust crates for aifix dependencies using curated indexes, ecosystem trackers, maintenance signals, coupling, feature footprint, security posture, and size risk.
condition: Use before adding a nontrivial Rust dependency or choosing between competing crates.
---

# Find Best Rust Crates

Use this skill before adding a nontrivial Rust dependency to `aifix`, or when a coding task needs a crate recommendation.

## Discovery sources

Check curated sources before plain keyword search:

* <https://blessed.rs/>: preferred first stop for common tasks with de facto standard crates.
* <https://github.com/rust-unofficial/awesome-rust>: broad category inventory and niche domains.
* <https://lib.rs>: compare health, maintenance status, categories, downloads, downstream users, dependency counts, and feature lists.
* <https://crates.guru>: useful secondary search for natural-language discovery.
* Ecosystem trackers when relevant:
  + <https://www.arewewebyet.org>
  + <https://arewegameyet.rs>
  + <https://areweguiyet.com>
  + <https://www.arewelearningyet.com>
  + <https://areweasyncyet.rs/>
  + <https://arewedistributedyet.com/>
  + <https://wiki.mozilla.org/Areweyet>
* Security and supply-chain sources when relevant:
  + <https://rustsec.org/advisories/>
  + <https://github.com/rustsec/advisory-db>
  + upstream repository security advisories and issue tracker
  + maintainer release notes for security-response evidence

## Selection procedure

1. State the job the crate must do and the coupling `aifix` can accept.
2. Find candidates from Blessed.rs, ecosystem trackers, awesome-rust, and lib.rs.
3. Compare at least the strongest two candidates unless there is a clear ecosystem-standard single choice.
4. Inspect lib.rs metadata and the upstream repository for health and fit.
5. Check recent RustSec and upstream security history for high-risk advisories, compromise reports, and response quality.
6. Prefer crates that keep `aifix` loosely coupled, backend-flexible, local-only, and compatible with current Rust APIs.
7. Prefer crates whose useful functionality works with minimal features and `default-features = false`.
8. Record the decision and rejected alternatives in the task report or the relevant project documentation.

## Relative quality scoring

Score candidates relative to the task, not as absolute endorsements.

Uprank crates that have:

* strong task fit with a small API surface;
* good support for current Rust APIs, including async only when async is relevant;
* loose coupling and backend flexibility;
* many downloads and active use;
* more than 20 downstream users;
* multiple maintainers or a credible contributor base;
* reputable primary contributors with proven ecosystem-level track records;
* optional dependencies for heavyweight integrations;
* clear docs, examples, and recent compatibility maintenance;
* no recent high-risk advisories, or a clear record of fast and transparent security response.

Downrank crates that have:

* low downloads for their age and domain;
* explicit unmaintained/deprecated status or signs of abandonment;
* more than 6 months between updates unless mature, feature-complete, and widely established;
* age under 3 months unless specialized and an exact fit;
* a single maintainer with few contributors;
* fewer than 5 downstream users;
* many required dependencies, especially if most are not optional;
* large binary or dependency-size impact;
* forced coupling to major ecosystems such as a runtime or framework when `aifix` needs a local, synchronous CLI boundary;
* popularity but outdated API assumptions, such as pre-async designs for async workloads;
* heavy procedural macro use where a simpler API would work;
* supersession by a better-maintained fork or successor, such as considering `winnow` before `nom` when appropriate;
* recent high-risk CVEs, supply-chain compromises, or slow security response.

## Output format

Report:

* recommended crate and version strategy;
* minimum feature set and why defaults stay disabled;
* strongest rejected alternatives and why they lost;
* maintenance, downloads, downstream users, and maintainer/contributor signals;
* security posture, recent advisories, and response history;
* coupling and backend risks;
* dependency and build-size risks;
* optional features that look relevant but stay disabled, with rationale.
