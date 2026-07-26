//! Agent-first diagnostic adapter library for `aifix`.
//!
//! The crate normalizes diagnostics from compiler, linter, LSP, and plain-text
//! tool outputs into one serde-compatible model. Callers can ingest existing
//! output with [`adapter::parse_diagnostics`], invoke configured tools through
//! [`batch`], build a grouped [`model::Digest`] with [`digest`], and render the
//! result for agents through [`render`].
//!
//! # Contract
//!
//! - ensures: parsing is deterministic and performs no network access;
//!   renderers consume normalized digests instead of reinterpreting raw tool
//!   payloads.
//! - provides: adapters, batch execution, digest construction, MCP, renderers,
//!   stable signatures, and normalized diagnostic data models.
//! - fails: boundary APIs represent parse, configuration, process, and tool
//!   failures as typed errors or normalized invocations.
//! - panics: none at the crate-contract boundary; item-specific panic behavior
//!   is documented on the item.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used,
        reason = "the Wyrd standard test-allow set keeps unit and property tests readable"
    )
)]

// Modules intentionally use `alloc::*` paths alongside `core` where possible.
extern crate alloc;

/// Diagnostic adapter entry points.
pub mod adapter;
/// Batch command execution support.
pub mod batch;
/// Project-local diagnostic cache and fix replay support.
pub mod cache;
/// Configuration loading and validation.
pub mod config;
/// Digest construction from normalized diagnostics.
pub mod digest;
/// Crate-specific error and result types.
pub mod error;
/// Deterministic explanation metadata for diagnostic groups.
pub mod explain;
mod lsp_fix;
/// Newline-delimited stdio Model Context Protocol server.
pub mod mcp;
/// Normalized diagnostic data model.
pub mod model;
/// Agent-facing renderers for normalized digests.
pub mod render;
/// Stable diagnostic signature construction and validation.
pub mod signature;
/// Syntax-context extraction for conservative diagnostic cache matching.
pub mod syntax;
