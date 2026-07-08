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
    /// Plain Agda `--interaction-json`-free text diagnostics.
    AgdaText,
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
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str
    {
        match self {
            | Self::Auto => "auto",
            | Self::AifixJson => "aifix-json",
            | Self::ClippyJson => "clippy-json",
            | Self::AgdaText => "agda-text",
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
    /// # Errors
    /// Returns formatter errors when writing the protocol spelling fails.
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
    /// - requires: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - ensures: returns the corresponding protocol for known spellings.
    /// - fails: returns [`AifixError::InvalidArgument`] for unknown spellings.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] for unknown protocol spellings.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        match s.trim().to_ascii_lowercase().as_str() {
            | "auto" => Ok(Self::Auto),
            | "aifix-json" | "aifix_json" | "aifix" => Ok(Self::AifixJson),
            | "clippy-json" | "clippy_json" | "rust-json" | "rustc-json" => Ok(Self::ClippyJson),
            | "agda-text" | "agda_text" | "agda" | "agda-cli" => Ok(Self::AgdaText),
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
    /// # Errors
    /// Returns formatter errors when writing the output-format spelling fails.
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
    /// - requires: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - ensures: returns the corresponding output format for known spellings.
    /// - fails: returns [`AifixError::InvalidArgument`] for unknown spellings.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] for unknown output-format
    /// spellings.
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
    /// # Errors
    /// Returns formatter errors when writing the severity spelling fails.
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
    /// - requires: `s` may contain surrounding whitespace and supported
    ///   aliases.
    /// - ensures: returns the corresponding normalized severity for known
    ///   spellings.
    /// - fails: returns [`AifixError::InvalidArgument`] for unknown spellings.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] for unknown severity spellings.
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

/// Outcome state for one profile considered by `batch auto`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileRunState
{
    /// The profile command ran and its output was parsed into diagnostics.
    Ran,
    /// The profile was considered but not applicable to the current project.
    Skipped,
    /// The profile was selected but failed before producing a parseable digest.
    Failed,
}

/// Metadata describing one profile considered by `batch auto`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRunStatus
{
    /// Profile name as selected by auto discovery.
    pub profile: String,
    /// Outcome for this profile.
    pub state: ProfileRunState,
    /// Protocol used or planned for parsing the profile output.
    pub protocol: Protocol,
    /// Human-readable command family, normally the executable name.
    pub command_family: String,
    /// Number of diagnostics produced by a successful run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_count: Option<usize>,
    /// Stable best-effort failure category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// Human-readable failure text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Human-readable skip or selection reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Complete normalized digest emitted by pipeline and batch modes.
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
    /// Per-profile status metadata for `batch auto` runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profile_statuses: Vec<ProfileRunStatus>,
}

impl Digest
{
    /// Truncate retained diagnostic samples without changing aggregate counts.
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
