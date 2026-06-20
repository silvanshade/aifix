//! Rust syntax context helpers for diagnostic cache matching.
//!
//! The helpers in this module parse only Rust source files through tree-sitter
//! and return bounded, deterministic fingerprints suitable for cache audit and
//! conservative replay matching.  Failures are represented as stable no-match
//! reasons so callers can degrade to exact-signature behavior without treating
//! missing parser context as fatal.

use alloc::borrow::ToOwned as _;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::fs;

use camino::Utf8Path;
use serde::Deserialize;
use serde::Serialize;
use tree_sitter::Node;
use tree_sitter::Parser;

use crate::error::AifixError;
use crate::model::Diagnostic;

/// Maximum source bytes read for syntax context.
const MAX_SOURCE_BYTES: usize = 0x10_0000;
/// Maximum diagnostic span bytes accepted for bounded evidence.
const MAX_SPAN_BYTES: usize = 0x4000;
/// Maximum normalized text bytes included before hashing a node.
const MAX_FINGERPRINT_TEXT_CHARS: usize = 96;
/// Maximum nearby sibling fingerprints retained on each side.
const SIBLING_RADIUS: usize = 2;

/// Result of trying to compute syntax context for a diagnostic.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyntaxContextResult
{
    /// Syntax evidence was computed for a supported Rust diagnostic span.
    Evidence(SyntaxContextEvidence),
    /// Syntax context is unavailable and callers should use exact matching.
    NoMatch
    {
        reason: String
    },
}

impl SyntaxContextResult
{
    /// Return the evidence when present.
    ///
    /// # Contract
    /// Preconditions: `self` is any syntax context result. Postconditions:
    /// returns `Some` only for evidence results. Failure modes: none. Panics:
    /// none.
    #[must_use]
    #[inline]
    pub const fn evidence(&self) -> Option<&SyntaxContextEvidence>
    {
        match *self {
            | Self::Evidence(ref value) => Some(value),
            | Self::NoMatch { .. } => None,
        }
    }

    /// Return the stable no-match reason when present.
    ///
    /// # Contract
    /// Preconditions: `self` is any syntax context result. Postconditions:
    /// returns `Some` only for no-match results. Failure modes: none. Panics:
    /// none.
    #[must_use]
    #[inline]
    pub fn reason(&self) -> Option<&str>
    {
        match *self {
            | Self::Evidence(_) => None,
            | Self::NoMatch { ref reason } => Some(reason.as_str()),
        }
    }
}

/// Bounded syntax evidence for a diagnostic primary span.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxContextEvidence
{
    /// Stable fingerprint for the selected syntax node.
    pub node: String,
    /// Stable fingerprint for the selected node's parent when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Stable fingerprints for nearby named siblings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nearby: Vec<String>,
    /// Zero-based byte offset where the selected node starts.
    pub node_byte_start: usize,
    /// Zero-based byte offset where the selected node ends.
    pub node_byte_end: usize,
    /// Signed byte delta from diagnostic span start to selected node start.
    pub span_start_byte_delta: isize,
    /// Signed byte delta from diagnostic span end to selected node end.
    pub span_end_byte_delta: isize,
    /// One-based line where the selected node starts.
    pub node_line_start: u32,
    /// One-based line where the selected node ends.
    pub node_line_end: u32,
    /// One-based line where the diagnostic span starts.
    pub span_line_start: u32,
    /// One-based line where the diagnostic span ends.
    pub span_line_end: u32,
    /// Detected line ending kind for the source file.
    pub line_ending: LineEndingKind,
    /// Leading-whitespace signal for the selected node line.
    pub leading_whitespace: LeadingWhitespaceSignal,
}

/// Source line-ending style observed while computing context.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEndingKind
{
    /// No line ending was observed.
    None,
    /// Unix line feeds were observed.
    Lf,
    /// Windows CRLF endings were observed.
    Crlf,
    /// Both LF and CRLF endings were observed.
    Mixed,
}

/// Bounded leading-whitespace signal for a source line.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LeadingWhitespaceSignal
{
    /// Number of leading space characters.
    pub spaces: usize,
    /// Number of leading tab characters.
    pub tabs: usize,
    /// Stable hash for the exact leading-whitespace bytes.
    pub fingerprint: String,
}

/// Compute Rust syntax context for a diagnostic primary span.
///
/// # Contract
/// Preconditions: `project_root` is a UTF-8 project directory and `diagnostic`
/// is normalized. Postconditions: only `.rs` paths are parsed; unsupported or
/// unavailable source returns a stable no-match reason instead of an error.
/// Failure modes: filesystem read errors return typed IO errors. Panics: none.
///
/// # Errors
/// Returns [`AifixError::Io`] when a supported source file cannot be read.
#[inline]
pub fn syntax_context_for_diagnostic(
    project_root: &Utf8Path,
    diagnostic: &Diagnostic,
) -> Result<SyntaxContextResult, AifixError>
{
    let Some(span) = diagnostic.spans.first()
    else {
        return Ok(no_match("missing-primary-span"));
    };
    let path = Utf8Path::new(span.file.as_str());
    if path.extension() != Some("rs") {
        return Ok(no_match("unsupported-source-path"));
    }
    let source_path = if path.is_absolute() {
        path.to_path_buf()
    }
    else {
        project_root.join(path)
    };
    let bytes = fs::read(&source_path)
        .map_err(|source| AifixError::io_path(source_path.clone(), source))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Ok(no_match("source-too-large"));
    }
    let Ok(source) = String::from_utf8(bytes)
    else {
        return Ok(no_match("source-not-utf8"));
    };
    context_from_source(
        &source,
        span.line,
        span.column,
        span.end_line,
        span.end_column,
    )
}

/// Build context from source text and one-based diagnostic coordinates.
fn context_from_source(
    source: &str,
    line: Option<u32>,
    column: Option<u32>,
    end_line: Option<u32>,
    end_column: Option<u32>,
) -> Result<SyntaxContextResult, AifixError>
{
    let Some(start_line) = line
    else {
        return Ok(no_match("missing-start-line"));
    };
    let Some(start_column) = column
    else {
        return Ok(no_match("missing-start-column"));
    };
    let finish_line = end_line.unwrap_or(start_line);
    let finish_column = end_column.unwrap_or_else(|| start_column.saturating_add(1));
    let Some(span_start) = byte_offset_for_position(source, start_line, start_column)
    else {
        return Ok(no_match("invalid-start-position"));
    };
    let Some(mut span_end) = byte_offset_for_position(source, finish_line, finish_column)
    else {
        return Ok(no_match("invalid-end-position"));
    };
    if span_end < span_start {
        return Ok(no_match("inverted-span"));
    }
    if span_end == span_start {
        span_end = span_end.saturating_add(1).min(source.len());
    }
    if span_end.saturating_sub(span_start) > MAX_SPAN_BYTES {
        return Ok(no_match("span-too-large"));
    }

    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    parser
        .set_language(&language)
        .map_err(|error| AifixError::parser(format!("tree-sitter rust language error: {error}")))?;
    let Some(tree) = parser.parse(source, None)
    else {
        return Ok(no_match("parse-failed"));
    };
    let root = tree.root_node();
    if root.has_error() {
        return Ok(no_match("parse-error"));
    }
    let selected = select_named_node(root, span_start, span_end);
    let node_line_start = line_for_byte(source, selected.start_byte());
    let node_line_end = line_for_byte(source, selected.end_byte());

    Ok(SyntaxContextResult::Evidence(SyntaxContextEvidence {
        node: node_fingerprint(selected, source),
        parent: selected
            .parent()
            .map(|parent| node_fingerprint(parent, source)),
        nearby: sibling_fingerprints(selected, source),
        node_byte_start: selected.start_byte(),
        node_byte_end: selected.end_byte(),
        span_start_byte_delta: signed_delta(selected.start_byte(), span_start),
        span_end_byte_delta: signed_delta(selected.end_byte(), span_end),
        node_line_start,
        node_line_end,
        span_line_start: start_line,
        span_line_end: finish_line,
        line_ending: detect_line_ending(source),
        leading_whitespace: leading_whitespace_for_line(source, node_line_start),
    }))
}

/// Return a stable no-match result.
fn no_match(reason: &str) -> SyntaxContextResult
{
    SyntaxContextResult::NoMatch {
        reason: reason.to_owned(),
    }
}

/// Select the smallest named node covering the diagnostic byte range.
fn select_named_node(
    root: Node<'_>,
    start: usize,
    end: usize,
) -> Node<'_>
{
    let mut selected = root
        .named_descendant_for_byte_range(start, end)
        .unwrap_or(root);
    while let Some(parent) = selected.parent() {
        if selected.start_byte() <= start && end <= selected.end_byte() {
            break;
        }
        selected = parent;
    }
    selected
}

/// Build nearby named sibling fingerprints around a node.
fn sibling_fingerprints(
    node: Node<'_>,
    source: &str,
) -> Vec<String>
{
    let mut siblings = Vec::new();
    let mut previous = node;
    for _ in 0 .. SIBLING_RADIUS {
        let Some(value) = previous.prev_named_sibling()
        else {
            break;
        };
        siblings.insert(0, node_fingerprint(value, source));
        previous = value;
    }
    let mut next = node;
    for _ in 0 .. SIBLING_RADIUS {
        let Some(value) = next.next_named_sibling()
        else {
            break;
        };
        siblings.push(node_fingerprint(value, source));
        next = value;
    }
    siblings
}

/// Fingerprint a tree-sitter node using kind and bounded normalized shape.
fn node_fingerprint(
    node: Node<'_>,
    source: &str,
) -> String
{
    let shape = if node.named_child_count() > 0 {
        child_shape(node)
    }
    else {
        normalized_leaf_text(node, source)
    };
    format!("{}:{}", node.kind(), stable_hash_hex(shape.as_bytes()))
}

/// Build a bounded child-kind shape string for non-leaf nodes.
fn child_shape(node: Node<'_>) -> String
{
    let mut shape = String::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor).take(8) {
        if !shape.is_empty() {
            shape.push('|');
        }
        shape.push_str(child.kind());
    }
    if node.named_child_count() > 8 {
        shape.push_str("|...");
    }
    shape
}

/// Normalize bounded leaf text without exposing raw payload identity.
fn normalized_leaf_text(
    node: Node<'_>,
    source: &str,
) -> String
{
    let mut normalized = String::new();
    let mut in_space = false;
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    let Some(text) = source.get(start .. end)
    else {
        return String::new();
    };
    for character in text.chars().take(MAX_FINGERPRINT_TEXT_CHARS) {
        if character.is_whitespace() {
            if !in_space {
                normalized.push(' ');
                in_space = true;
            }
        }
        else if character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '!') {
            normalized.push(character);
            in_space = false;
        }
        else {
            normalized.push('#');
            in_space = false;
        }
    }
    normalized
}

/// Convert one-based line/column coordinates to a byte offset.
fn byte_offset_for_position(
    source: &str,
    line: u32,
    column: u32,
) -> Option<usize>
{
    if line == 0 || column == 0 {
        return None;
    }
    let mut current_line = 1_u32;
    let mut line_start = 0_usize;
    for (index, character) in source.char_indices() {
        if current_line == line {
            break;
        }
        if character == '\n' {
            current_line = current_line.saturating_add(1);
            line_start = index.saturating_add(1);
        }
    }
    if current_line != line {
        return None;
    }
    let target_column = column.saturating_sub(1);
    let mut current_column = 0_u32;
    let line_text = source.get(line_start ..)?;
    for (offset, character) in line_text.char_indices() {
        if current_column == target_column {
            return Some(line_start.saturating_add(offset));
        }
        if character == '\n' {
            break;
        }
        current_column = current_column.saturating_add(1);
    }
    (current_column == target_column).then_some(source.len())
}

/// Return a one-based line number for a byte offset.
fn line_for_byte(
    source: &str,
    byte: usize,
) -> u32
{
    let mut line = 1_u32;
    for (index, character) in source.char_indices() {
        if index >= byte {
            break;
        }
        if character == '\n' {
            line = line.saturating_add(1);
        }
    }
    line
}

/// Detect source line-ending style.
fn detect_line_ending(source: &str) -> LineEndingKind
{
    let has_crlf = source.as_bytes().windows(2).any(|window| window == b"\r\n");
    let has_lf = source.lines().count() > 1 || source.ends_with('\n');
    match (has_lf, has_crlf) {
        | (false, false) => LineEndingKind::None,
        | (true, false) => LineEndingKind::Lf,
        | (false, true) => LineEndingKind::Crlf,
        | (true, true) => LineEndingKind::Mixed,
    }
}

/// Return leading-whitespace evidence for a one-based line.
fn leading_whitespace_for_line(
    source: &str,
    line: u32,
) -> LeadingWhitespaceSignal
{
    let mut current_line = 1_u32;
    let mut line_start = 0_usize;
    for (index, character) in source.char_indices() {
        if current_line == line {
            break;
        }
        if character == '\n' {
            current_line = current_line.saturating_add(1);
            line_start = index.saturating_add(1);
        }
    }
    let text = source.get(line_start ..).unwrap_or("");
    let mut spaces = 0_usize;
    let mut tabs = 0_usize;
    let mut bytes = Vec::new();
    for character in text.chars() {
        match character {
            | ' ' => {
                spaces = spaces.saturating_add(1);
                bytes.push(b' ');
            },
            | '\t' => {
                tabs = tabs.saturating_add(1);
                bytes.push(b'\t');
            },
            | _ => break,
        }
    }
    LeadingWhitespaceSignal {
        spaces,
        tabs,
        fingerprint: stable_hash_hex(&bytes),
    }
}

/// Compute a signed delta between two byte offsets.
fn signed_delta(
    value: usize,
    base: usize,
) -> isize
{
    if value >= base {
        isize::try_from(value.saturating_sub(base)).unwrap_or(isize::MAX)
    }
    else {
        -isize::try_from(base.saturating_sub(value)).unwrap_or(isize::MAX)
    }
}

/// Compute deterministic FNV-1a hex for bounded syntax identity.
fn stable_hash_hex(bytes: &[u8]) -> String
{
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3_u64);
    }
    format!("{hash:016x}")
}
