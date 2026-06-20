//! Agent-first diagnostic adapter library for `aifix`.
//!
//! The crate normalizes diagnostics from compiler, linter, LSP, and plain-text
//! tool outputs into one serde-compatible model.  Callers can ingest existing
//! output with [`adapter::parse_diagnostics`], invoke configured tools through
//! [`batch`], build a grouped [`model::Digest`] with [`digest`], and render the
//! result for agents through [`render`].
//!
//! # Contracts
//!
//! - Parsing is deterministic and performs no network access.
//! - Non-zero tool exits are represented in [`model::Invocation`] rather than
//!   treated as fatal when diagnostics were still parsed.
//! - Renderers consume the normalized digest model and do not reinterpret raw
//!   tool payloads.

// Modules intentionally use `alloc::*` paths alongside `core` where possible.
extern crate alloc;

/// Diagnostic adapter entry points.
///
/// # Contract
/// - Preconditions: adapter callers provide UTF-8 diagnostic payloads.
/// - Postconditions: payloads are normalized into model diagnostics without
///   invoking tools.
/// - Failure modes: parsing functions return typed errors.
/// - Panics: none.
pub mod adapter;
/// Batch command execution support.
///
/// # Contract
/// - Preconditions: callers provide validated batch configuration.
/// - Postconditions: invocation output is represented in the normalized model.
/// - Failure modes: IO and process errors are returned through the crate error
///   type.
/// - Panics: none.
pub mod batch;
/// Project-local diagnostic cache and fix replay support.
///
/// # Contract
/// - Preconditions: callers provide a UTF-8 project root or allow the current
///   directory to be used.
/// - Postconditions: cache helpers persist deterministic JSON at
///   `.aifix/diagnostics.json` and replay patches through direct `git` argv.
/// - Failure modes: IO, JSON, process, and invalid-argument failures are
///   returned through the crate error type.
/// - Panics: none.
pub mod cache;
/// Configuration loading and validation.
///
/// # Contract
/// - Preconditions: configuration sources are UTF-8 and TOML-compatible when
///   present.
/// - Postconditions: validated configuration is exposed to pipeline code.
/// - Failure modes: configuration and parse errors are returned through the
///   crate error type.
/// - Panics: none.
pub mod config;
/// Digest construction from normalized diagnostics.
///
/// # Contract
/// - Preconditions: callers provide normalized diagnostics and invocation
///   metadata.
/// - Postconditions: diagnostics are deduplicated, counted, grouped, and
///   explained deterministically.
/// - Failure modes: construction returns typed errors for invalid arguments.
/// - Panics: none.
pub mod digest;
/// Crate-specific error and result types.
///
/// # Contract
/// - Preconditions: callers preserve boundary context when constructing errors.
/// - Postconditions: each failure category remains typed and displayable.
/// - Failure modes: none for the module item itself.
/// - Panics: none.
pub mod error;
/// Deterministic explanation metadata for diagnostic groups.
///
/// # Contract
/// - Preconditions: callers provide grouped diagnostic context.
/// - Postconditions: explanation records are stable and allocation-conscious.
/// - Failure modes: none for the module item itself.
/// - Panics: none.
pub mod explain;
/// Newline-delimited stdio Model Context Protocol server.
///
/// # Contract
/// - Preconditions: callers connect UTF-8 stdin and writable stdout following
///   JSON-RPC 2.0 line framing.
/// - Postconditions: server requests are answered with exactly one JSON object
///   per response line and no stdout logging.
/// - Failure modes: IO failures and response serialization errors are returned
///   through the crate error type; tool failures are encoded as MCP tool
///   results.
/// - Panics: none.
pub mod mcp;
/// Normalized diagnostic data model.
///
/// # Contract
/// - Preconditions: adapters provide already-normalized strings and
///   coordinates.
/// - Postconditions: serde-compatible structs preserve diagnostics,
///   invocations, counts, and groups.
/// - Failure modes: parsing helpers return typed errors.
/// - Panics: none.
pub mod model;
/// Agent-facing renderers for normalized digests.
///
/// # Contract
/// - Preconditions: callers provide a complete digest and output-format
///   selection.
/// - Postconditions: renderers emit deterministic JSON or Markdown without
///   reinterpreting raw payloads.
/// - Failure modes: serialization errors are returned through the crate error
///   type.
/// - Panics: none.
pub mod render;
/// Stable diagnostic signature construction and validation.
///
/// # Contract
/// - Preconditions: callers provide normalized diagnostics or signature strings
///   shaped as `aifix-v1-<16 hex primary>-<16 hex secondary>`.
/// - Postconditions: signatures are deterministic over semantic diagnostic
///   fields and exclude raw payloads from identity.
/// - Failure modes: malformed signature strings return typed invalid-argument
///   errors.
/// - Panics: none.
pub mod signature;
/// Syntax-context extraction for conservative diagnostic cache matching.
///
/// # Contract
/// - Preconditions: callers provide a project root and normalized diagnostic.
/// - Postconditions: Rust source spans may produce bounded deterministic syntax
///   evidence; unsupported or unavailable context returns stable no-match
///   reasons.
/// - Failure modes: supported source read failures return typed errors.
/// - Panics: none.
pub mod syntax;
