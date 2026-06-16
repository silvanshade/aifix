# agent guidance

This is the initial agent guidance for the aifix project.

## Project Name

You are to implement a Rust CLI tool tentatively called aifix.

* Note the name is not stable until project is public.
* Perform brief search for name suitability.
* Suggest (but do not use) alternative names if necessary.

## Project Scope

aifix is an agent-first adapter that enables agents to more quickly and more accurately fix issues identified by tooling like linters, static analyzers, and LSP servers.

### Concepts and Design

Initially aifix will be designed as a CLI adapter executed on-demand.

A `datum` is any logic unit of information relevant for a particular tool interface.

Examples:

* LSP diagnostics
* lint suggestions
* error codes

A `datum` may be atomic (primitive, such as an integer) or compound (a JSON object).

The `data` emitted by a particular tool that `aifix` understands is expected to conform to some protocol.

#### Operation

aifix should offer two modes:

* pipeline mode: tool data ingest, digest for llm agent
* batch mode: invoke tools directly, tool data ingest, digest for llm agent

Both modes should offer a concise and focused set of configuration options to control behavior, along with reading from both a user-level and project-level config file.

#### Protocols and Formats

Protocol preferences:

* binary formats (faster, zero-copy)
* structured textual formats (JSON)
* ad-hoc formats with parse adapters (winnow based)

#### Concrete Features

* fix application prompting and guidance
  + allow sub-agent to help schedule fixes
  + allow sub-agent to explain (see explanation of tools below)
* deduplication of tool data
* minimization of tool data
* grouping and prioritization of tool data
* explanation of tool data
  + Use existing docs where possible (rustc and clippy --explain)
  + Allow tldr style templates otherwise
  + Allow batched agent-advisor queries to explain specific issues
    - Small/fast agent asks "what does X, Y, Z lint mean, with examples?"

#### Inspiration

The previous guidance is _suggested_ but the implementing agent is free to elaborate on the design and features according its own judgement.

However, concrete inspiration should take strong design cues from existing user/agent design scripts at:

* ~/Development/mach/.omp/
* ~/Development/wyrd/scripts/
* ~/Development/omp-prompt-ui/.omp/
* ~/Development/omp-prompt-ui/scripts/

The idea is to _generalize_ the patterns presented there, implement them more efficiently in Rust, make it more configurable and extensible, and provide some default tooling interface for at least the cases the user has already defined:

* nushell
* rust
* typescript

#### Task Execution

The implementing agent is strongly advised to perform some research on prior art for such a tool before starting.
Likely there has been _something_ beyond just the scripts mentioned previously.

#### Project Conventions

The agent should recall memories regarding user preferences for project configuration, then examine the following projects in detail and follow their overall structure as closely as possible (in order of preference):

* ~/Development/omp-prompt-ui
* ~/Deveopment/wyrd
* ~/Development/mach

Some specific points to follow:

* Issue uses local beads tracking (already configured remote available at <https://www.dolthub.com/repositories/silvanshade/aifix-beads>)
* Include _all_ of the project-level configuration files which are relevant to the project, examples:
  + mise.toml (and all the other relevant toml files)
  + treefmt.toml (follow the full configuration for relevant tools)
  + rumdl.toml (follow the exact configuration)
* Follow the ket documentation discipline (see ~/Development/wyrd/docs/KNOWLEDGE.md)
* Follow all other CAPS-level .md documents where relevant
* Maintain CHANGELOG.md (with git-cliff)
* Maintain docs/ADR.md (with adrs) (use skill://architecture-decision-records to orient)
* Follow the conventions around crates from ~/Development/mach/crates/**
  + should produce all of the same docs as seen in ~/Development/mach/crates/ouroborosh-tree-sitter/docs/_.md
* beads should reference ADRs, this should also potentially be folded into ket discipline if you can find a nice way
* scan ~/Development/omp-prompt-ui beads for pending workflow tasks, and ~/Development/wyrd beads for past decisions to inform yourself
* All source code should be fully documented (including rust private items) and follow a design-by-contract discipline:
  + document pre and post conditions and failure mode or error behavior
  + document panics
  + follow mach and wyrd guidance on allowance for overriding lints
  + use exact Rust configs where possible from mach
* project should aim to include tests, close to as complete coverage as possible, fuzzing, benchmarks
* include some informative tests and potentially some examples to run that demonstrate the tool working as expected.

#### Deliverables

- the working crate CLI tool, with some examples
- a review pass with skill://improve-codebase-architecture followed by a refinement pass
- a completed adversarial analysis followed by refinement pass
- this AGENTS.md file replaced with a proper post-project-init guidance

#### Notes

The agent may invoke skill://grill-with-docs prior to implementation work if deemed useful
