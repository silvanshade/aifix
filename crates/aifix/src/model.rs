//! Normalized diagnostic data model.
//!
//! The model is intentionally small and serde-compatible.  Adapters populate
//! these types from source-specific payloads, digest construction deduplicates
//! and groups them, and renderers emit agent-facing guidance.

use alloc::borrow::ToOwned as _;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use camino::Utf8Path;
use serde::Deserialize;
use serde::Serialize;

use crate::error::AifixError;
use crate::explain::Explain;

/// Input protocol understood by the diagnostic adapter.
///
/// # Contract
/// - Preconditions: values are selected by callers or parsed from stable CLI
///   spellings.
/// - Postconditions: each variant maps to one adapter strategy and serde
///   kebab-case spelling.
/// - Failure modes: none while held as a value; parsing failures are reported
///   by [`FromStr`].
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol
{
    /// Infer the protocol from the input shape.
    Auto,
    /// Native normalized `aifix` JSON.
    AifixJson,
    /// Cargo/rustc/clippy newline-delimited compiler-message JSON.
    ClippyJson,
    /// Plain TypeScript compiler diagnostics.
    TypescriptText,
    /// JSON Language Server Protocol diagnostics.
    LspJson,
    /// Nushell linter text or generic non-empty diagnostic lines.
    NushellText,
}

impl Protocol
{
    /// Return the stable CLI spelling for this protocol.
    ///
    /// # Contract
    /// - Preconditions: `self` is any supported protocol variant.
    /// - Postconditions: returns a non-empty static kebab-case spelling
    ///   accepted by [`FromStr`].
    /// - Failure modes: none.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str
    {
        match self {
            | Self::Auto => "auto",
            | Self::AifixJson => "aifix-json",
            | Self::ClippyJson => "clippy-json",
            | Self::TypescriptText => "typescript-text",
            | Self::LspJson => "lsp-json",
            | Self::NushellText => "nushell-text",
        }
    }
}

impl fmt::Display for Protocol
{
    /// Format using the stable CLI spelling.
    ///
    /// # Contract
    /// - Preconditions: `f` is writable by the formatting runtime.
    /// - Postconditions: writes exactly [`Protocol::as_str`] for `self`.
    /// - Failure modes: returns the formatter error if writing fails.
    /// - Panics: none.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        f.write_str(self.as_str())
    }
}

impl FromStr for Protocol
{
    type Err = AifixError;

    /// Parse a stable protocol spelling.
    ///
    /// # Contract
    /// - Preconditions: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - Postconditions: returns the corresponding protocol for known
    ///   spellings.
    /// - Failure modes: returns [`AifixError::InvalidArgument`] for unknown
    ///   spellings.
    /// - Panics: none.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        match s.trim().to_ascii_lowercase().as_str() {
            | "auto" => Ok(Self::Auto),
            | "aifix-json" | "aifix_json" | "aifix" => Ok(Self::AifixJson),
            | "clippy-json" | "clippy_json" | "rust-json" | "rustc-json" => Ok(Self::ClippyJson),
            | "typescript-text" | "typescript_text" | "tsc" | "tsc-text" => {
                Ok(Self::TypescriptText)
            },
            | "lsp-json" | "lsp_json" | "lsp" => Ok(Self::LspJson),
            | "nushell-text" | "nushell_text" | "nu" | "nu-text" => Ok(Self::NushellText),
            | other => Err(AifixError::invalid_argument(format!(
                "unknown protocol `{other}`"
            ))),
        }
    }
}

/// Digest output representation.
///
/// # Contract
/// - Preconditions: values are selected by callers or parsed from stable CLI
///   spellings.
/// - Postconditions: each variant maps to one renderer format and serde
///   kebab-case spelling.
/// - Failure modes: none while held as a value; parsing failures are reported
///   by [`FromStr`].
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat
{
    /// Full pretty JSON including raw diagnostic payloads.
    Json,
    /// Compact JSON with raw payloads omitted and samples capped.
    CompactJson,
    /// Markdown guidance optimized for repair agents.
    Markdown,
}

impl OutputFormat
{
    /// Return the stable CLI spelling for this output format.
    ///
    /// # Contract
    /// - Preconditions: `self` is any supported output-format variant.
    /// - Postconditions: returns a non-empty static kebab-case spelling
    ///   accepted by [`FromStr`].
    /// - Failure modes: none.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str
    {
        match self {
            | Self::Json => "json",
            | Self::CompactJson => "compact-json",
            | Self::Markdown => "markdown",
        }
    }
}

impl fmt::Display for OutputFormat
{
    /// Format using the stable CLI spelling.
    ///
    /// # Contract
    /// - Preconditions: `f` is writable by the formatting runtime.
    /// - Postconditions: writes exactly [`OutputFormat::as_str`] for `self`.
    /// - Failure modes: returns the formatter error if writing fails.
    /// - Panics: none.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        f.write_str(self.as_str())
    }
}

impl FromStr for OutputFormat
{
    type Err = AifixError;

    /// Parse a stable output-format spelling.
    ///
    /// # Contract
    /// - Preconditions: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - Postconditions: returns the corresponding output format for known
    ///   spellings.
    /// - Failure modes: returns [`AifixError::InvalidArgument`] for unknown
    ///   spellings.
    /// - Panics: none.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        match s.trim().to_ascii_lowercase().as_str() {
            | "json" => Ok(Self::Json),
            | "compact-json" | "compact_json" => Ok(Self::CompactJson),
            | "markdown" | "md" => Ok(Self::Markdown),
            | other => Err(AifixError::invalid_argument(format!(
                "unknown output format `{other}`"
            ))),
        }
    }
}

/// Normalized diagnostic severity ordered from least to most urgent.
///
/// # Contract
/// - Preconditions: source tools may use richer or protocol-specific
///   severities.
/// - Postconditions: values collapse severities into the four levels used by
///   grouping and rendering.
/// - Failure modes: none while held as a value; parsing failures are reported
///   by [`FromStr`].
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity
{
    /// Advisory information with no direct correctness impact.
    Hint,
    /// Informational diagnostic.
    Info,
    /// Warning that may still allow the tool invocation to complete.
    Warning,
    /// Error that usually blocks compilation, checking, or lint success.
    Error,
}

impl Severity
{
    /// Return the stable lowercase spelling for this severity.
    ///
    /// # Contract
    /// - Preconditions: `self` is any supported severity variant.
    /// - Postconditions: returns a non-empty static lowercase spelling accepted
    ///   by [`FromStr`].
    /// - Failure modes: none.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str
    {
        match self {
            | Self::Hint => "hint",
            | Self::Info => "info",
            | Self::Warning => "warning",
            | Self::Error => "error",
        }
    }

    /// Convert a source-tool severity string into a normalized severity.
    ///
    /// # Contract
    /// - Preconditions: `value` is a severity label from an external tool.
    /// - Postconditions: returns error, warning, hint, or info with unknown
    ///   labels treated as info.
    /// - Failure modes: none.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn from_tool_str(value: &str) -> Self
    {
        match value.trim().to_ascii_lowercase().as_str() {
            | "error" | "fatal" => Self::Error,
            | "warning" | "warn" => Self::Warning,
            | "hint" => Self::Hint,
            | _ => Self::Info,
        }
    }
}

impl fmt::Display for Severity
{
    /// Format using the stable lowercase spelling.
    ///
    /// # Contract
    /// - Preconditions: `f` is writable by the formatting runtime.
    /// - Postconditions: writes exactly [`Severity::as_str`] for `self`.
    /// - Failure modes: returns the formatter error if writing fails.
    /// - Panics: none.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity
{
    type Err = AifixError;

    /// Parse a normalized severity spelling.
    ///
    /// # Contract
    /// - Preconditions: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - Postconditions: returns the corresponding normalized severity for
    ///   known spellings.
    /// - Failure modes: returns [`AifixError::InvalidArgument`] for unknown
    ///   spellings.
    /// - Panics: none.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        match s.trim().to_ascii_lowercase().as_str() {
            | "hint" => Ok(Self::Hint),
            | "info" | "note" | "help" => Ok(Self::Info),
            | "warning" | "warn" => Ok(Self::Warning),
            | "error" | "fatal" => Ok(Self::Error),
            | other => Err(AifixError::invalid_argument(format!(
                "unknown severity `{other}`"
            ))),
        }
    }
}

/// Source-code span associated with a diagnostic.
///
/// # Contract
/// - Preconditions: coordinates, when present, are one-based and describe a
///   single source location.
/// - Postconditions: serde omits absent coordinates while preserving file or
///   URI text.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Span
{
    /// File path or URI reported by the source tool.
    pub file: String,
    /// One-based start line when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// One-based start column when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// One-based end line when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// One-based end column when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
}

impl Span
{
    /// Construct a span with optional one-based coordinates.
    ///
    /// # Contract
    /// - Preconditions: coordinates are one-based when present; start is not
    ///   after end for well-formed source spans.
    /// - Postconditions: stores the provided file text and coordinates
    ///   unchanged.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn new<File>(
        file: File,
        line: Option<u32>,
        column: Option<u32>,
        end_line: Option<u32>,
        end_column: Option<u32>,
    ) -> Self
    where
        File: Into<String>,
    {
        let span = Self {
            file: file.into(),
            line,
            column,
            end_line,
            end_column,
        };
        if let (Some(start_line), Some(finish_line)) = (span.line, span.end_line)
            && start_line == finish_line
            && let (Some(start_column), Some(finish_column)) = (span.column, span.end_column)
        {
            debug_assert!(
                start_column <= finish_column,
                "span start column must not exceed end column on one line"
            );
        }
        span
    }
}

/// Suggested source edit or human action attached to a diagnostic.
///
/// # Contract
/// - Preconditions: `message` is human-readable and non-empty for normalized
///   suggestions.
/// - Postconditions: serde omits absent machine replacement and span fields.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Suggestion
{
    /// Human-readable suggestion text.
    pub message: String,
    /// Replacement text, when the tool supplied a machine-applicable edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
    /// Span to which the suggestion applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
}

impl Suggestion
{
    /// Construct a diagnostic suggestion.
    ///
    /// # Contract
    /// - Preconditions: `message` is non-empty after conversion for normalized
    ///   suggestions.
    /// - Postconditions: stores message, optional replacement, and optional
    ///   span unchanged.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn new<Message>(
        message: Message,
        replacement: Option<String>,
        span: Option<Span>,
    ) -> Self
    where
        Message: Into<String>,
    {
        let suggestion = Self {
            message: message.into(),
            replacement,
            span,
        };
        debug_assert!(
            !suggestion.message.trim().is_empty(),
            "normalized suggestion messages must be non-empty"
        );
        suggestion
    }
}

/// One normalized diagnostic emitted by an adapter.
///
/// Preserved raw JSON keeps source-specific evidence available to renderers
/// that choose to expose it, but digest duplicate detection intentionally uses
/// only the normalized semantic fields.
///
/// # Contract
/// - Preconditions: `source` and `message` are non-empty after adapter
///   normalization.
/// - Postconditions: serde omits absent optional fields and empty detail
///   vectors; raw payloads do not define semantic diagnostic identity.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic
{
    /// Source tool or protocol family, such as `rustc`, `clippy`, or `tsc`.
    pub source: String,
    /// Structured diagnostic code when the source tool supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Normalized severity.
    pub severity: Severity,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Source spans attached to the diagnostic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    /// Source suggestions attached to the diagnostic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// Raw JSON payload preserved for structured adapters; excluded from
    /// digest fingerprinting and grouping identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl Diagnostic
{
    /// Construct a diagnostic without spans, suggestions, or raw payload.
    ///
    /// # Contract
    /// - Preconditions: `source` and `message` are non-empty after conversion.
    /// - Postconditions: creates a diagnostic with empty spans, empty
    ///   suggestions, and no raw payload.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn new<Source, Message>(
        source: Source,
        code: Option<String>,
        severity: Severity,
        message: Message,
    ) -> Self
    where
        Source: Into<String>,
        Message: Into<String>,
    {
        let diagnostic = Self {
            source: source.into(),
            code,
            severity,
            message: message.into(),
            spans: Vec::new(),
            suggestions: Vec::new(),
            raw: None,
        };
        debug_assert!(
            !diagnostic.source.trim().is_empty(),
            "normalized diagnostic source must be non-empty"
        );
        debug_assert!(
            !diagnostic.message.trim().is_empty(),
            "normalized diagnostic message must be non-empty"
        );
        diagnostic
    }

    /// Attach spans, suggestions, and raw JSON in one allocation-conscious
    /// step.
    ///
    /// # Contract
    /// - Preconditions: detail vectors already describe this diagnostic and
    ///   contain normalized items.
    /// - Postconditions: replaces the existing detail fields without changing
    ///   source, severity, code, or message.
    /// - Failure modes: none; ownership is moved into `self`.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn with_details(
        mut self,
        spans: Vec<Span>,
        suggestions: Vec<Suggestion>,
        raw: Option<serde_json::Value>,
    ) -> Self
    {
        self.spans = spans;
        self.suggestions = suggestions;
        self.raw = raw;
        self
    }
}

/// Metadata for the tool invocation that produced diagnostics.
///
/// # Contract
/// - Preconditions: command entries and captured streams are already UTF-8.
/// - Postconditions: serde omits absent cwd, empty streams, and absent exit
///   code.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation
{
    /// Executable and argv, or a synthetic pipeline descriptor.
    pub command: Vec<String>,
    /// Working directory used for command execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Captured standard output.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    /// Captured standard error.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    /// Process exit code, when a process was invoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl Invocation
{
    /// Construct invocation metadata from command output without exposing
    /// struct literals.
    ///
    /// # Contract
    /// - Preconditions: `command` describes the executed program or synthetic
    ///   source and is non-empty when process-like metadata is available; each
    ///   command entry is non-empty; streams are UTF-8.
    /// - Postconditions: converts and stores every field without changing the
    ///   serde shape of [`Invocation`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: debug builds may panic if `command` is empty or contains an
    ///   empty entry.
    #[must_use]
    #[inline]
    pub fn new<Command, Cwd, Stdout, Stderr>(
        command: Command,
        cwd: Option<Cwd>,
        stdout: Stdout,
        stderr: Stderr,
        exit_code: Option<i32>,
    ) -> Self
    where
        Command: Into<Vec<String>>,
        Cwd: Into<String>,
        Stdout: Into<String>,
        Stderr: Into<String>,
    {
        let command = command.into();
        debug_assert!(
            !command.is_empty(),
            "invocation command should contain at least one entry"
        );
        debug_assert!(
            command.iter().all(|entry| !entry.is_empty()),
            "invocation command entries should be non-empty"
        );
        Self {
            command,
            cwd: cwd.map(Into::into),
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code,
        }
    }

    /// Construct synthetic invocation metadata for pipeline mode.
    ///
    /// # Contract
    /// - Preconditions: `protocol` is the parser used for `input`; `input` is
    ///   the original pipeline payload label.
    /// - Postconditions: builds `["aifix", "pipeline", protocol, input]` with
    ///   empty streams and no cwd or exit code.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn pipeline<Input>(
        protocol: Protocol,
        input: Input,
    ) -> Self
    where
        Input: Into<String>,
    {
        let invocation = Self {
            command: vec![
                "aifix".to_owned(),
                "pipeline".to_owned(),
                protocol.as_str().to_owned(),
                input.into(),
            ],
            cwd: None,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        };
        debug_assert_eq!(
            invocation.command.len(),
            4,
            "pipeline invocation must preserve command shape"
        );
        invocation
    }

    /// Construct invocation metadata from a UTF-8 working directory path.
    ///
    /// # Contract
    /// - Preconditions: `cwd` is the working directory used for the command and
    ///   is valid UTF-8; `command` is non-empty and contains no empty entries.
    /// - Postconditions: stores `cwd.as_str()` as the invocation cwd and
    ///   preserves other fields.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: debug builds may panic if `command` is empty or contains an
    ///   empty entry.
    #[must_use]
    #[inline]
    pub fn with_cwd_path(
        command: Vec<String>,
        cwd: &Utf8Path,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    ) -> Self
    {
        Self::new(
            command,
            Some(cwd.as_str().to_owned()),
            stdout,
            stderr,
            exit_code,
        )
    }
}

/// Aggregate diagnostic counts.
///
/// # Contract
/// - Preconditions: count maps are computed from the same deduplicated
///   diagnostics as `total`.
/// - Postconditions: serde preserves deterministic map ordering through
///   [`BTreeMap`].
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts
{
    /// Total number of deduplicated diagnostics.
    pub total: usize,
    /// Counts by normalized severity.
    pub by_severity: BTreeMap<Severity, usize>,
    /// Counts by diagnostic source name.
    pub by_source: BTreeMap<String, usize>,
}

/// Group of related diagnostics sharing a code or message fallback.
///
/// # Contract
/// - Preconditions: `source` is non-empty and `count` is the full group size
///   before sample truncation.
/// - Postconditions: samples may be bounded independently from `count`;
///   explanation metadata describes the group.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group
{
    /// Diagnostic source name.
    pub source: String,
    /// Structured code shared by the group, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Message fallback used when the group has no structured code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Number of deduplicated diagnostics in the group.
    pub count: usize,
    /// Highest severity observed in the group.
    pub severity: Severity,
    /// Sample diagnostics retained for agent context.
    pub diagnostics: Vec<Diagnostic>,
    /// Deterministic explanation metadata for the group.
    pub explain: Explain,
}

/// Complete normalized digest emitted by pipeline and batch modes.
///
/// # Contract
/// - Preconditions: counts, groups, and diagnostics were computed from the same
///   normalized invocation output.
/// - Postconditions: carries both aggregate/grouped views and the full
///   deduplicated diagnostic list.
/// - Failure modes: none while held as a value.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest
{
    /// Invocation metadata for the source data.
    pub invocation: Invocation,
    /// Aggregate counts for all deduplicated diagnostics.
    pub counts: Counts,
    /// Grouped diagnostics with bounded samples.
    pub groups: Vec<Group>,
    /// Full deduplicated diagnostic list.
    pub diagnostics: Vec<Diagnostic>,
}

impl Digest
{
    /// Truncate retained diagnostic samples without changing aggregate counts.
    ///
    /// # Contract
    /// - Preconditions: `max_diagnostics` is the requested retained sample cap;
    ///   `None` means no truncation.
    /// - Postconditions: top-level diagnostics and every group sample list have
    ///   length at most the cap when present.
    /// - Failure modes: none; vector truncation is infallible.
    /// - Panics: none.
    #[inline]
    pub fn truncate_samples(
        &mut self,
        max_diagnostics: Option<usize>,
    )
    {
        if let Some(limit) = max_diagnostics {
            self.diagnostics.truncate(limit);
            debug_assert!(
                self.diagnostics.len() <= limit,
                "top-level diagnostics must respect sample limit"
            );
            for group in &mut self.groups {
                group.diagnostics.truncate(limit);
                debug_assert!(
                    group.diagnostics.len() <= limit,
                    "group diagnostics must respect sample limit"
                );
            }
        }
    }
}
