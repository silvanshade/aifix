//! Render digests as JSON, compact JSON, or Markdown.
//!
//! Full JSON preserves the digest exactly.  Compact JSON uses borrowed mirror
//! structs so raw tool payloads are omitted without mutating the source digest.

use serde::Serialize;

use crate::error::AifixError;
use crate::explain::Explain;
use crate::model::Counts;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Group;
use crate::model::Invocation;
use crate::model::OutputFormat;
use crate::model::Severity;
use crate::model::Span;
use crate::model::Suggestion;

/// Default compact-mode sample cap per group.
const DEFAULT_COMPACT_SAMPLE_LIMIT: usize = 3;

/// Render a digest in the requested output format.
///
/// # Contract
/// - requires: `digest` is a fully constructed model value.
/// - ensures: returns the selected representation using the default compact
///   sample cap when compact JSON is requested.
/// - fails: returns JSON serialization errors for JSON formats.
/// - panics: debug builds may panic if the selected renderer violates
///   documented output invariants.
///
/// # Errors
/// Returns an error when JSON or compact JSON serialization fails.
#[inline]
pub fn render_digest(
    digest: &Digest,
    format: OutputFormat,
) -> Result<String, AifixError>
{
    render_digest_with_sample_limit(digest, format, DEFAULT_COMPACT_SAMPLE_LIMIT)
}

/// Render a digest, overriding compact JSON's sample cap.
///
/// # Contract
/// - requires: `digest` is a fully constructed model value and
///   `compact_sample_limit` is the desired per-group compact sample cap.
/// - ensures: preserves full JSON exactly, omits raw payloads in compact JSON,
///   and emits Markdown text for Markdown output.
/// - fails: returns JSON serialization errors for JSON formats.
/// - panics: debug builds may panic if the selected renderer violates
///   documented output or compact sample invariants.
///
/// # Errors
/// Returns an error when JSON or compact JSON serialization fails.
#[inline]
pub fn render_digest_with_sample_limit(
    digest: &Digest,
    format: OutputFormat,
    compact_sample_limit: usize,
) -> Result<String, AifixError>
{
    match format {
        | OutputFormat::Json => serde_json::to_string_pretty(digest).map_err(AifixError::Json),
        | OutputFormat::CompactJson => render_compact_json(digest, compact_sample_limit),
        | OutputFormat::Markdown => Ok(render_markdown(digest)),
    }
}

/// Render compact JSON with no raw diagnostic payloads.
///
/// # Contract
/// - requires: `digest` is a fully constructed model value.
/// - ensures: serializes a compact borrowed mirror whose group samples are
///   capped by `sample_limit` and whose invocation omits stdout/stderr bodies.
/// - fails: returns a JSON serialization error when serialization fails.
/// - panics: debug builds may panic if compact group construction violates the
///   sample cap invariant.
///
/// # Errors
/// Returns an error when compact JSON serialization fails.
fn render_compact_json(
    digest: &Digest,
    sample_limit: usize,
) -> Result<String, AifixError>
{
    let compact = CompactDigest::new(digest, sample_limit);
    serde_json::to_string(&compact).map_err(AifixError::Json)
}

/// Render grouped Markdown guidance for agents.
///
/// # Contract
/// - requires: `digest` is a fully constructed model value.
/// - ensures: returns Markdown containing invocation, count, group, explain,
///   and sample sections without mutating the digest.
/// - panics: debug builds may panic if the Markdown heading or trailing newline
///   invariant is violated.
fn render_markdown(digest: &Digest) -> String
{
    let mut markdown = String::new();
    markdown.push_str("# aifix diagnostic digest\n\n");
    markdown.push_str("## Invocation\n\n");
    markdown.push_str("- Command: ");
    markdown.push_str(&shell_words(&digest.invocation.command));
    markdown.push('\n');
    if let Some(cwd) = digest.invocation.cwd.as_deref() {
        markdown.push_str("- Cwd: ");
        markdown.push_str(cwd);
        markdown.push('\n');
    }
    if let Some(exit_code) = digest.invocation.exit_code {
        markdown.push_str("- Exit code: ");
        markdown.push_str(&exit_code.to_string());
        markdown.push('\n');
    }

    markdown.push_str("\n## Counts\n\n");
    markdown.push_str("- Total diagnostics: ");
    markdown.push_str(&digest.counts.total.to_string());
    markdown.push('\n');
    for (severity, count) in &digest.counts.by_severity {
        markdown.push_str("- ");
        markdown.push_str(severity_label(*severity));
        markdown.push_str(": ");
        markdown.push_str(&count.to_string());
        markdown.push('\n');
    }
    if !digest.profile_statuses.is_empty() {
        markdown.push_str("\n## Profile statuses\n\n");
        for status in &digest.profile_statuses {
            markdown.push_str("- `");
            markdown.push_str(&status.profile);
            markdown.push_str("`: ");
            markdown.push_str(match status.state {
                | crate::model::ProfileRunState::Ran => "ran",
                | crate::model::ProfileRunState::Skipped => "skipped",
                | crate::model::ProfileRunState::Failed => "failed",
            });
            markdown.push_str(" (protocol `");
            markdown.push_str(status.protocol.as_str());
            markdown.push_str("`, command family `");
            markdown.push_str(&status.command_family);
            markdown.push_str("`)");
            if let Some(count) = status.diagnostic_count {
                markdown.push_str(": ");
                markdown.push_str(&count.to_string());
                markdown.push_str(" diagnostics");
            }
            if let Some(reason) = status.reason.as_deref() {
                markdown.push_str(" — ");
                markdown.push_str(reason);
            }
            if let Some(error_kind) = status.error_kind.as_deref() {
                markdown.push_str(" [");
                markdown.push_str(error_kind);
                markdown.push(']');
            }
            if let Some(error) = status.error.as_deref() {
                markdown.push_str(": ");
                markdown.push_str(&error.replace('\n', " "));
            }
            markdown.push('\n');
        }
    }

    markdown.push_str("\n## Groups\n");
    for group in &digest.groups {
        markdown.push('\n');
        markdown.push_str("### ");
        markdown.push_str(&group_heading(group));
        markdown.push('\n');
        markdown.push_str("- Count: ");
        markdown.push_str(&group.count.to_string());
        markdown.push('\n');
        markdown.push_str("- Severity: ");
        markdown.push_str(severity_label(group.severity));
        markdown.push('\n');
        markdown.push_str("- Explain: ");
        markdown.push_str(&group.explain.summary);
        markdown.push_str(" (`");
        markdown.push_str(&group.explain.explain_ref);
        markdown.push_str("`)\n");

        let retained_samples = group.diagnostics.len();
        if retained_samples > 0 {
            markdown.push_str("- Samples:\n");
            for diagnostic in &group.diagnostics {
                markdown.push_str("  - ");
                markdown.push_str(&diagnostic.message.replace('\n', " "));
                if let Some(span) = diagnostic.spans.first() {
                    markdown.push_str(" — ");
                    markdown.push_str(&format_span(span));
                }
                markdown.push('\n');
            }
        }
        if group.count > retained_samples {
            markdown.push_str("- Hidden samples: ");
            markdown.push_str(&(group.count - retained_samples).to_string());
            markdown.push_str(" (retained ");
            markdown.push_str(&retained_samples.to_string());
            markdown.push_str(" of ");
            markdown.push_str(&group.count.to_string());
            markdown.push_str(" diagnostics; increase maxDiagnostics to show more)\n");
        }
    }

    debug_assert!(
        markdown.starts_with("# aifix diagnostic digest\n\n"),
        "markdown renderer must emit the digest heading"
    );
    debug_assert!(
        markdown.ends_with('\n'),
        "markdown renderer should finish on a line boundary"
    );

    markdown
}

/// Compact digest representation that omits raw JSON payloads.
#[derive(Serialize)]
struct CompactDigest<'digest>
{
    /// Preserved invocation metadata.
    invocation: CompactInvocation<'digest>,
    /// Preserved aggregate counts.
    counts: &'digest Counts,
    /// Profile status metadata for auto runs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    profile_statuses: &'digest Vec<crate::model::ProfileRunStatus>,
    /// Compact grouped diagnostics.
    groups: Vec<CompactGroup<'digest>>,
}

/// Borrowing constructors for compact digest rendering.
impl<'digest> CompactDigest<'digest>
{
    /// Borrow compact data from a full digest.
    ///
    /// # Contract
    /// - requires: `digest` is valid for `'digest` and `sample_limit` is the
    ///   desired per-group cap.
    /// - ensures: returns a borrowed compact mirror with one compact group per
    ///   source group.
    /// - panics: debug builds may panic if compact group construction violates
    ///   the sample cap invariant.
    fn new(
        digest: &'digest Digest,
        sample_limit: usize,
    ) -> Self
    {
        Self {
            invocation: CompactInvocation::new(&digest.invocation),
            counts: &digest.counts,
            profile_statuses: &digest.profile_statuses,
            groups: digest
                .groups
                .iter()
                .map(|group| CompactGroup::new(group, sample_limit))
                .collect(),
        }
    }
}

/// Compact invocation metadata without captured stdout/stderr bodies.
#[derive(Serialize)]
struct CompactInvocation<'digest>
{
    /// Executable and arguments.
    command: &'digest [String],
    /// Working directory used for the process, when known.
    cwd: Option<&'digest str>,
    /// Process exit code, when the platform reported one.
    exit_code: Option<i32>,
    /// Complete stdout byte length, including omitted invocation bytes.
    stdout_bytes: usize,
    /// Complete stderr byte length, including omitted invocation bytes.
    stderr_bytes: usize,
}

/// Borrowing constructors for compact invocation rendering.
impl<'digest> CompactInvocation<'digest>
{
    /// Borrow compact invocation metadata from a full invocation.
    fn new(invocation: &'digest Invocation) -> Self
    {
        Self {
            command: &invocation.command,
            cwd: invocation.cwd.as_deref(),
            exit_code: invocation.exit_code,
            stdout_bytes: invocation.total_stdout_bytes(),
            stderr_bytes: invocation.total_stderr_bytes(),
        }
    }
}

/// Compact group representation with capped samples.
#[derive(Serialize)]
struct CompactGroup<'digest>
{
    /// Diagnostic source name.
    source: &'digest str,
    /// Structured code shared by the group, when present.
    code: Option<&'digest str>,
    /// Message fallback for codeless groups.
    message: Option<&'digest str>,
    /// Number of diagnostics represented by this group.
    count: usize,
    /// Highest severity observed in the group.
    severity: Severity,
    /// Deterministic explanation metadata.
    explain: &'digest Explain,
    /// Capped sample diagnostics with raw payloads removed.
    diagnostics: Vec<CompactDiagnostic<'digest>>,
}

/// Borrowing constructors for compact group rendering.
impl<'digest> CompactGroup<'digest>
{
    /// Borrow compact group data from a full group.
    ///
    /// # Contract
    /// - requires: `group` is valid for `'digest` and `sample_limit` is the
    ///   desired per-group cap.
    /// - ensures: returns borrowed group metadata with diagnostics capped to at
    ///   most `sample_limit`.
    /// - panics: debug builds may panic if compact diagnostics exceed the
    ///   requested sample cap.
    fn new(
        group: &'digest Group,
        sample_limit: usize,
    ) -> Self
    {
        let diagnostics = group
            .diagnostics
            .iter()
            .take(sample_limit)
            .map(CompactDiagnostic::new)
            .collect::<Vec<_>>();
        debug_assert!(
            diagnostics.len() <= sample_limit,
            "compact diagnostics must respect sample cap"
        );

        Self {
            source: &group.source,
            code: group.code.as_deref(),
            message: group.message.as_deref(),
            count: group.count,
            severity: group.severity,
            explain: &group.explain,
            diagnostics,
        }
    }
}

/// Compact diagnostic representation without raw tool JSON.
#[derive(Serialize)]
struct CompactDiagnostic<'digest>
{
    /// Diagnostic source name.
    source: &'digest str,
    /// Structured diagnostic code, when present.
    code: Option<&'digest str>,
    /// Normalized severity.
    severity: Severity,
    /// Human-readable diagnostic message.
    message: &'digest str,
    /// Source spans attached to the diagnostic.
    spans: &'digest [Span],
    /// Tool suggestions attached to the diagnostic.
    suggestions: &'digest [Suggestion],
}

/// Borrowing constructors for compact diagnostic rendering.
impl<'digest> CompactDiagnostic<'digest>
{
    /// Borrow compact diagnostic data from a full diagnostic.
    fn new(diagnostic: &'digest Diagnostic) -> Self
    {
        Self {
            source: &diagnostic.source,
            code: diagnostic.code.as_deref(),
            severity: diagnostic.severity,
            message: &diagnostic.message,
            spans: &diagnostic.spans,
            suggestions: &diagnostic.suggestions,
        }
    }
}

/// Build a concise group heading.
///
/// # Contract
/// - requires: `group` is a normalized digest group.
/// - ensures: returns a single-line heading using code when present, codeless
///   message fallback when present, and source otherwise.
/// - panics: none.
fn group_heading(group: &Group) -> String
{
    match (group.code.as_deref(), group.message.as_deref()) {
        | (Some(code), _) => format!("{} {code}", group.source),
        | (None, Some(message)) => format!("{} {}", group.source, message.replace('\n', " ")),
        | (None, None) => group.source.clone(),
    }
}

/// Render one source span for Markdown.
///
/// # Contract
/// - requires: `span` is a normalized source span.
/// - ensures: returns `file`, `file:line`, or `file:line:column` depending on
///   available position metadata.
/// - panics: none.
fn format_span(span: &Span) -> String
{
    let mut rendered = span.file.clone();
    if let Some(line) = span.line {
        rendered.push(':');
        rendered.push_str(&line.to_string());
        if let Some(column) = span.column {
            rendered.push(':');
            rendered.push_str(&column.to_string());
        }
    }
    rendered
}

/// Render a command vector without invoking a shell.
///
/// # Contract
/// - requires: `command` contains already split argv parts and may be empty for
///   stdin-style invocations.
/// - ensures: returns `<stdin>` for an empty command or a shell-readable
///   display string with unsafe words single-quoted.
/// - panics: none.
fn shell_words(command: &[String]) -> String
{
    if command.is_empty() {
        return "<stdin>".to_owned();
    }

    command
        .iter()
        .map(|part| {
            if part.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.' | ':' | '=')
            }) {
                part.clone()
            }
            else {
                format!("'{escaped}'", escaped = part.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Human-readable severity label.
fn severity_label(severity: Severity) -> &'static str
{
    match severity {
        | Severity::Hint => "hint",
        | Severity::Info => "info",
        | Severity::Warning => "warning",
        | Severity::Error => "error",
    }
}
