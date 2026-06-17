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
