//! Diagnostic protocol adapters.
//!
//! Adapters translate source-tool output into [`crate::model::Diagnostic`]
//! values without invoking tools or reaching the network.  Structured JSON
//! adapters preserve their raw JSON payload so downstream agents can inspect
//! source-specific details when the normalized fields are insufficient.

use std::io::BufRead;

use serde_json::Value;

use crate::error::AifixError;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Protocol;
use crate::model::Severity;
use crate::model::Span;
use crate::model::Suggestion;

/// Result of probing one adapter while inferring the diagnostic protocol.
enum AutoProbe
{
    /// The probed input does not have this adapter's shape.
    NoMatch,
    /// The probed adapter matched and normalized diagnostics.
    Matched(Vec<Diagnostic>),
    /// The probed adapter matched but rejected malformed input.
    Invalid(AifixError),
}

impl AutoProbe
{
    /// Convert a parser result into a matched-or-invalid probe result.
    ///
    /// # Contract
    /// - requires: the caller has already established that the adapter shape is
    ///   present.
    /// - ensures: preserves successful diagnostics and parser errors without
    ///   falling through to generic text parsing.
    /// - fails: returns [`AutoProbe::Invalid`] when `result` is an error.
    /// - panics: none.
    #[inline]
    fn from_result(result: Result<Vec<Diagnostic>, AifixError>) -> Self
    {
        match result {
            | Ok(diagnostics) => Self::Matched(diagnostics),
            | Err(error) => Self::Invalid(error),
        }
    }
}

/// Parse diagnostics from `input` according to `protocol`.
///
/// # Contract
/// - requires: `input` is UTF-8 output from the selected diagnostic protocol.
/// - ensures: returns deterministic normalized diagnostics without invoking
///   tools or network access.
/// - fails: returns [`AifixError::Json`] for malformed JSON protocols or
///   [`AifixError::Parser`] when non-empty input contains no diagnostics.
/// - panics: none.
///
/// # Errors
/// Returns [`AifixError::Json`] when a selected JSON protocol receives
/// malformed JSON. Returns [`AifixError::Parser`] when non-empty input cannot
/// be normalized by the selected protocol.
#[inline]
pub fn parse_diagnostics(
    protocol: Protocol,
    input: &str,
) -> Result<Vec<Diagnostic>, AifixError>
{
    match protocol {
        | Protocol::Auto => parse_auto(input),
        | Protocol::AifixJson => parse_aifix_json(input),
        | Protocol::ClippyJson => parse_clippy_json(input),
        | Protocol::AgdaText => parse_agda_text(input),
        | Protocol::TypescriptText => parse_typescript_text(input),
        | Protocol::LspJson => parse_lsp_json(input),
        | Protocol::NushellText => parse_nushell_text(input),
    }
}
/// Parse diagnostics from a buffered reader according to `protocol`.
///
/// # Contract
/// - requires: `reader` yields UTF-8 output from the selected diagnostic
///   protocol.
/// - ensures: explicitly selected newline-delimited cargo, Agda, TypeScript,
///   and generic text stream one record at a time; structured JSON decodes
///   directly from the reader. `Auto` preserves the string parser contract for
///   non-replayable callers; batch capture detects auto input in a separate
///   bounded-memory pass before dispatch.
/// - fails: returns [`AifixError::Io`] for read failures, JSON errors for
///   malformed structured input, or parser errors for unsupported input.
/// - panics: none.
///
/// # Errors
/// Returns an error when the reader fails or the selected adapter rejects the
/// diagnostic input.
#[inline]
pub fn parse_diagnostics_reader<Reader>(
    protocol: Protocol,
    mut reader: Reader,
) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    match protocol {
        | Protocol::Auto => {
            let mut input = String::new();
            reader.read_to_string(&mut input).map_err(AifixError::io)?;
            parse_auto(&input)
        },
        | Protocol::AifixJson => {
            let value = serde_json::from_reader(reader)?;
            parse_aifix_value(&value)
        },
        | Protocol::ClippyJson => parse_clippy_json_reader(&mut reader),
        | Protocol::AgdaText => parse_agda_text_reader(reader),
        | Protocol::TypescriptText => parse_typescript_text_reader(reader),
        | Protocol::LspJson => {
            let value = serde_json::from_reader(reader)?;
            parse_lsp_value(&value)
        },
        | Protocol::NushellText => parse_nushell_text_reader(reader),
    }
}

/// Auto-detection result for replayable batch input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoReaderProtocol
{
    /// Replay the input through the selected streaming adapter.
    Selected(Protocol),
    /// Decode one complete JSON value directly from the replayed input.
    CompleteJson,
}

/// Detect an auto protocol in one bounded-memory pass over replayable input.
///
/// # Contract
/// - requires: `reader` yields UTF-8 input that can be reopened by the caller.
/// - ensures: preserves auto precedence: cargo JSONL, complete JSON, Agda,
///   TypeScript, then generic text.
/// - fails: returns an IO error when reading fails.
/// - panics: none.
///
/// # Errors
/// Returns an error when the reader cannot be consumed.
pub(crate) fn detect_auto_protocol_reader<Reader>(
    reader: Reader
) -> Result<AutoReaderProtocol, AifixError>
where
    Reader: BufRead,
{
    let mut saw_cargo = false;
    let mut saw_complete_json_prefix = false;
    let mut saw_first_non_empty = false;
    let mut saw_agda = false;
    let mut saw_typescript = false;

    for line in reader.lines() {
        let line = line.map_err(AifixError::io)?;
        let trimmed = line.trim();
        if !saw_first_non_empty && !trimmed.is_empty() {
            saw_first_non_empty = true;
            saw_complete_json_prefix = matches!(trimmed.as_bytes().first(), Some(b'{' | b'['));
        }
        saw_cargo |= is_cargo_json_line(&line);
        saw_agda |= parse_agda_header(&line).is_some() || is_agda_status_line(&line);
        saw_typescript |= parse_typescript_line(&line).is_some();
    }

    if saw_cargo {
        Ok(AutoReaderProtocol::Selected(Protocol::ClippyJson))
    }
    else if saw_complete_json_prefix {
        Ok(AutoReaderProtocol::CompleteJson)
    }
    else if saw_agda {
        Ok(AutoReaderProtocol::Selected(Protocol::AgdaText))
    }
    else if saw_typescript {
        Ok(AutoReaderProtocol::Selected(Protocol::TypescriptText))
    }
    else {
        Ok(AutoReaderProtocol::Selected(Protocol::NushellText))
    }
}

/// Parse one auto-detected complete JSON payload without an intermediate
/// string.
///
/// # Errors
/// Returns JSON or parser errors for malformed or unsupported structured input.
pub(crate) fn parse_complete_json_reader<Reader>(
    reader: Reader
) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    let value = serde_json::from_reader(reader)?;
    if lsp_json_shape(&value) {
        parse_lsp_value(&value)
    }
    else {
        parse_aifix_value(&value)
    }
}

/// Try adapters in an order that rejects malformed structured shapes.
///
/// # Contract
/// - requires: `input` is UTF-8 diagnostic output and may be empty.
/// - ensures: empty or whitespace-only input yields an empty vector;
///   structured-looking inputs are handled by their matched adapter, while Agda
///   text is detected before unstructured output falls back to TypeScript then
///   generic diagnostics.
/// - fails: returns the matched structured parser error instead of silently
///   converting malformed JSON, LSP, or cargo shapes to generic text.
/// - panics: none.
fn parse_auto(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }

    match probe_cargo_json(input) {
        | AutoProbe::Matched(diagnostics) => return Ok(diagnostics),
        | AutoProbe::Invalid(error) => return Err(error),
        | AutoProbe::NoMatch => {},
    }

    match probe_complete_json(input) {
        | AutoProbe::Matched(diagnostics) => return Ok(diagnostics),
        | AutoProbe::Invalid(error) => return Err(error),
        | AutoProbe::NoMatch => {},
    }

    match probe_agda_text(input) {
        | AutoProbe::Matched(diagnostics) => return Ok(diagnostics),
        | AutoProbe::Invalid(error) => return Err(error),
        | AutoProbe::NoMatch => {},
    }

    if let Ok(diagnostics) = parse_typescript_text(input) {
        return Ok(diagnostics);
    }

    parse_nushell_text(input)
}

/// Probe cargo newline-delimited JSON when cargo-shaped fields are present.
///
/// # Contract
/// - requires: `input` is non-empty UTF-8 text.
/// - ensures: returns [`AutoProbe::NoMatch`] unless a cargo compiler-message
///   shape marker is present.
/// - fails: malformed cargo-shaped JSON becomes [`AutoProbe::Invalid`] so auto
///   mode cannot fall through to generic lines.
/// - panics: none.
fn probe_cargo_json(input: &str) -> AutoProbe
{
    if input.lines().any(is_cargo_json_line) {
        AutoProbe::from_result(parse_clippy_json(input))
    }
    else {
        AutoProbe::NoMatch
    }
}

/// Probe complete JSON payloads for native and LSP diagnostic shapes.
///
/// # Contract
/// - requires: `input` is non-empty UTF-8 text.
/// - ensures: returns [`AutoProbe::NoMatch`] for non-JSON-looking text;
///   otherwise a supported structured match or an invalid structured error.
/// - fails: malformed or unsupported JSON-looking diagnostics become
///   [`AutoProbe::Invalid`] instead of generic text diagnostics.
/// - panics: none.
fn probe_complete_json(input: &str) -> AutoProbe
{
    if !looks_like_complete_json(input) {
        return AutoProbe::NoMatch;
    }

    let value = match parse_json_value(input) {
        | Ok(value) => value,
        | Err(error) => return AutoProbe::Invalid(error),
    };

    if lsp_json_shape(&value) {
        return AutoProbe::from_result(parse_lsp_value(&value));
    }

    match parse_aifix_value(&value) {
        | Ok(diagnostics) => AutoProbe::Matched(diagnostics),
        | Err(error) => {
            if value.is_array() || value.is_object() {
                AutoProbe::Invalid(error)
            }
            else {
                AutoProbe::NoMatch
            }
        },
    }
}

/// Parse native normalized `aifix` JSON.
///
/// # Contract
/// - requires: `input` contains a JSON value produced by `aifix` or matching
///   its normalized model.
/// - ensures: returns diagnostics from a digest, diagnostic array, single
///   diagnostic, or diagnostics property.
/// - fails: returns JSON errors for malformed JSON or parser errors when no
///   diagnostics shape is present.
/// - panics: none.
fn parse_aifix_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_aifix_value(&parse_json_value(input)?)
}

/// Convert native normalized `aifix` JSON into diagnostics.
///
/// # Contract
/// - requires: `value` is a parsed JSON value produced by `aifix` or matching
///   its normalized model.
/// - ensures: returns diagnostics from a digest, diagnostic array, single
///   diagnostic, or diagnostics property.
/// - fails: returns JSON errors for malformed model-shaped values or a parser
///   error when no diagnostics shape is present.
/// - panics: none.
fn parse_aifix_value(value: &Value) -> Result<Vec<Diagnostic>, AifixError>
{
    if let Ok(digest) = serde_json::from_value::<Digest>(value.to_owned()) {
        return Ok(digest.diagnostics);
    }
    if let Ok(diagnostics) = serde_json::from_value::<Vec<Diagnostic>>(value.to_owned()) {
        return Ok(diagnostics);
    }
    if let Ok(diagnostic) = serde_json::from_value::<Diagnostic>(value.to_owned()) {
        return Ok(vec![diagnostic]);
    }
    if let Some(diagnostics) = value.get("diagnostics") {
        let parsed = serde_json::from_value::<Vec<Diagnostic>>(diagnostics.clone())?;
        return Ok(parsed);
    }

    Err(AifixError::parser("aifix JSON did not contain diagnostics"))
}

/// Incremental state for newline-delimited cargo compiler messages.
struct ClippyJsonParser
{
    /// Normalized diagnostics recovered from valid compiler-message records.
    diagnostics: Vec<Diagnostic>,
    /// Whether any non-whitespace input record was observed.
    saw_nonempty: bool,
    /// Whether any structurally valid Cargo JSON record was observed.
    saw_json: bool,
    /// Whether any valid JSON record had the compiler-message reason.
    saw_compiler_message: bool,
    /// First structured failure retained for deterministic error reporting.
    first_structured_error: Option<AifixError>,
}

impl ClippyJsonParser
{
    /// Construct empty parser state.
    ///
    /// # Contract
    /// - ensures: no records or diagnostics have been observed.
    /// - fails: none.
    /// - panics: none.
    #[must_use]
    #[inline]
    fn new() -> Self
    {
        Self {
            diagnostics: Vec::new(),
            saw_nonempty: false,
            saw_json: false,
            saw_compiler_message: false,
            first_structured_error: None,
        }
    }

    /// Consume one possibly noisy cargo-output line.
    ///
    /// # Contract
    /// - requires: `line` is one UTF-8 record without assumptions about
    ///   surrounding records.
    /// - ensures: ignores blank and non-compiler cargo records, retains valid
    ///   diagnostics, and records only the first structured failure.
    /// - fails: none; recoverable per-record failures are retained in state.
    /// - panics: none.
    fn push_line(
        &mut self,
        line: &str,
    )
    {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.saw_nonempty = true;

        let value = match serde_json::from_str::<Value>(line) {
            | Ok(value) => value,
            | Err(error) => {
                if (line.starts_with('{') || line.starts_with('['))
                    && self.first_structured_error.is_none()
                {
                    self.first_structured_error = Some(AifixError::Json(error));
                }
                return;
            },
        };
        let Some(reason) = value.get("reason").and_then(Value::as_str)
        else {
            if self.first_structured_error.is_none() {
                self.first_structured_error = Some(AifixError::parser(
                    "cargo JSON record missing string reason",
                ));
            }
            return;
        };
        if reason != "compiler-message" {
            if Self::is_valid_non_diagnostic_cargo_event(reason, &value) {
                self.saw_json = true;
            }
            else if self.first_structured_error.is_none() {
                self.first_structured_error = Some(AifixError::parser(format!(
                    "malformed or unsupported cargo JSON record `{reason}`"
                )));
            }
            return;
        }
        self.saw_json = true;
        self.saw_compiler_message = true;
        let Some(message) = value.get("message")
        else {
            if self.first_structured_error.is_none() {
                self.first_structured_error = Some(AifixError::parser(
                    "cargo compiler-message missing message object",
                ));
            }
            return;
        };
        let Some(diagnostic) = compiler_message_to_diagnostic(message, value.clone())
        else {
            if self.first_structured_error.is_none() {
                self.first_structured_error = Some(AifixError::parser(
                    "cargo compiler-message contained no non-empty message text",
                ));
            }
            return;
        };
        self.diagnostics.push(diagnostic);
    }

    /// Return whether a non-diagnostic Cargo JSON event has its required shape.
    #[must_use]
    fn is_valid_non_diagnostic_cargo_event(
        reason: &str,
        value: &Value,
    ) -> bool
    {
        match reason {
            | "compiler-artifact" => {
                value.get("package_id").is_some_and(Value::is_string)
                    && value.get("manifest_path").is_some_and(Value::is_string)
                    && value.get("target").is_some_and(Self::is_valid_cargo_target)
                    && value
                        .get("profile")
                        .is_some_and(Self::is_valid_cargo_profile)
                    && value.get("features").is_some_and(Self::is_string_array)
                    && value.get("filenames").is_some_and(Self::is_string_array)
                    && value
                        .get("executable")
                        .is_some_and(|executable| executable.is_null() || executable.is_string())
                    && value.get("fresh").is_some_and(Value::is_boolean)
            },
            | "build-script-executed" => {
                value.get("package_id").is_some_and(Value::is_string)
                    && value.get("linked_libs").is_some_and(Self::is_string_array)
                    && value.get("linked_paths").is_some_and(Self::is_string_array)
                    && value.get("cfgs").is_some_and(Self::is_string_array)
                    && value.get("env").is_some_and(Self::is_string_pair_array)
                    && value.get("out_dir").is_some_and(Value::is_string)
            },
            | "build-finished" => value.get("success").is_some_and(Value::is_boolean),
            | _ => false,
        }
    }

    /// Return whether a Cargo artifact target has the documented field types.
    #[must_use]
    fn is_valid_cargo_target(value: &Value) -> bool
    {
        value.get("kind").is_some_and(Self::is_string_array)
            && value.get("crate_types").is_some_and(Self::is_string_array)
            && value.get("name").is_some_and(Value::is_string)
            && value.get("src_path").is_some_and(Value::is_string)
            && value.get("edition").is_some_and(Value::is_string)
            && value.get("doc").is_some_and(Value::is_boolean)
            && value.get("doctest").is_some_and(Value::is_boolean)
            && value.get("test").is_some_and(Value::is_boolean)
    }

    /// Return whether a Cargo artifact profile has the documented field types.
    #[must_use]
    fn is_valid_cargo_profile(value: &Value) -> bool
    {
        value.get("opt_level").is_some_and(Value::is_string)
            && value.get("debuginfo").is_some_and(|debuginfo| {
                debuginfo.is_null()
                    || debuginfo.as_u64().is_some_and(|level| level <= 2)
                    || matches!(
                        debuginfo.as_str(),
                        Some("line-directives-only" | "line-tables-only")
                    )
            })
            && value.get("debug_assertions").is_some_and(Value::is_boolean)
            && value.get("overflow_checks").is_some_and(Value::is_boolean)
            && value.get("test").is_some_and(Value::is_boolean)
    }

    /// Return whether every element of a JSON array is a string.
    #[must_use]
    fn is_string_array(value: &Value) -> bool
    {
        value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    }

    /// Return whether every element is a two-string Cargo environment pair.
    #[must_use]
    fn is_string_pair_array(value: &Value) -> bool
    {
        value.as_array().is_some_and(|pairs| {
            pairs.iter().all(|pair| {
                pair.as_array()
                    .is_some_and(|items| items.len() == 2 && items.iter().all(Value::is_string))
            })
        })
    }

    /// Finalize cargo parsing after all records have been consumed.
    ///
    /// # Contract
    /// - ensures: valid diagnostics take precedence over adjacent malformed
    ///   records; empty, whitespace-only, and valid non-diagnostic cargo
    ///   streams succeed empty.
    /// - fails: returns the first structured error when no diagnostic was
    ///   recovered, or a parser error for non-empty input without Cargo JSON.
    /// - panics: none.
    fn finish(self) -> Result<Vec<Diagnostic>, AifixError>
    {
        if !self.diagnostics.is_empty() {
            return Ok(self.diagnostics);
        }
        if let Some(error) = self.first_structured_error {
            return Err(error);
        }
        if self.saw_nonempty && !self.saw_json {
            return Err(AifixError::parser(
                "clippy JSON input did not contain cargo JSON records",
            ));
        }
        Ok(self.diagnostics)
    }
}

/// Parse newline-delimited cargo compiler messages from a buffered reader.
///
/// # Contract
/// - requires: `reader` yields UTF-8 cargo output.
/// - ensures: retains parser state proportional to normalized diagnostics plus
///   the largest individual input line, not total input bytes.
/// - fails: returns IO, JSON, or parser errors under the same conditions as
///   [`parse_clippy_json`].
/// - panics: none.
fn parse_clippy_json_reader<Reader>(reader: &mut Reader) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    let mut parser = ClippyJsonParser::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(AifixError::io)?;
        if read == 0 {
            return parser.finish();
        }
        parser.push_line(&line);
    }
}

/// Parse newline-delimited cargo compiler-message JSON.
///
/// # Contract
/// - requires: non-empty lines may contain cargo JSON events, unrelated cargo
///   output, or truncated/noisy stream fragments.
/// - ensures: returns normalized compiler-message diagnostics, ignores other
///   cargo message reasons, and retains valid diagnostics even when adjacent
///   lines are malformed.
/// - fails: returns the first JSON/parser error when no valid compiler-message
///   diagnostic can be recovered.
/// - panics: none.
fn parse_clippy_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    let mut parser = ClippyJsonParser::new();
    for line in input.lines() {
        parser.push_line(line);
    }
    parser.finish()
}

/// Convert one rustc compiler message object into a normalized diagnostic.
///
/// # Contract
/// - requires: `message` is the `message` object from a cargo
///   `compiler-message` event and `raw` is the original event.
/// - ensures: returns a diagnostic with a non-empty message and preserved raw
///   JSON when a message is available.
/// - fails: returns `None` when the message text is absent or blank.
/// - panics: none.
fn compiler_message_to_diagnostic(
    message: &Value,
    raw: Value,
) -> Option<Diagnostic>
{
    let rendered = first_non_empty_string(message, &["rendered"]);
    let message_text = first_non_empty_string(message, &["message"]).or(rendered)?;
    debug_assert!(
        !message_text.trim().is_empty(),
        "compiler diagnostic message must be non-empty"
    );
    let code = message
        .get("code")
        .and_then(|code| first_non_empty_string(code, &["code"]));
    let source = rust_source_name(code.as_deref(), &message_text);
    debug_assert!(
        !source.is_empty(),
        "Rust diagnostic source must be non-empty"
    );
    let severity = message
        .get("level")
        .and_then(Value::as_str)
        .map_or(Severity::Error, Severity::from_tool_str);
    let spans = compiler_spans(message.get("spans"));
    let suggestions = compiler_suggestions(message.get("spans"));

    Some(
        Diagnostic::new(source, code, severity, message_text).with_details(
            spans,
            suggestions,
            Some(raw),
        ),
    )
}

/// Determine whether a Rust diagnostic came from clippy or rustc.
fn rust_source_name(
    code: Option<&str>,
    message: &str,
) -> &'static str
{
    debug_assert!(
        !message.trim().is_empty(),
        "Rust source classification needs a non-empty message"
    );
    if code.is_some_and(|value| value.starts_with("clippy::")) || message.contains("clippy") {
        "clippy"
    }
    else {
        "rustc"
    }
}

/// Extract primary compiler spans.
///
/// # Contract
/// - requires: `spans` is the optional rustc `spans` JSON array.
/// - ensures: returns only primary spans that can be normalized without direct
///   indexing.
/// - fails: malformed or missing spans are skipped.
/// - panics: none.
fn compiler_spans(spans: Option<&Value>) -> Vec<Span>
{
    spans
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter(|span| {
                    span.get("is_primary")
                        .and_then(Value::as_bool)
                        .is_some_and(core::convert::identity)
                })
                .filter_map(compiler_span)
                .collect()
        })
}

/// Extract machine suggestions from compiler spans.
///
/// # Contract
/// - requires: `spans` is the optional rustc `spans` JSON array.
/// - ensures: returns suggestions for spans with `suggested_replacement`,
///   preserving span data when available.
/// - fails: spans without replacement text are skipped.
/// - panics: none.
fn compiler_suggestions(spans: Option<&Value>) -> Vec<Suggestion>
{
    spans
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(|span| {
                    let replacement = first_string(span, &["suggested_replacement"])?;
                    let message = match first_non_empty_string(span, &["label"]) {
                        | Some(label) => label,
                        | None => "suggested replacement".to_owned(),
                    };
                    Some(Suggestion::new(
                        message,
                        Some(replacement),
                        compiler_span(span),
                    ))
                })
                .collect()
        })
}

/// Convert one compiler span object to a normalized span.
///
/// # Contract
/// - requires: `span` is a rustc span object with a `file_name` string.
/// - ensures: returns a span with one-based rustc coordinates copied when
///   present.
/// - fails: returns `None` when `file_name` is missing or not a string.
/// - panics: none.
fn compiler_span(span: &Value) -> Option<Span>
{
    let file = first_non_empty_string(span, &["file_name"])?;
    debug_assert!(
        !file.trim().is_empty(),
        "compiler span file names should be non-empty"
    );
    Some(Span::new(
        file,
        json_u32(span.get("line_start")),
        json_u32(span.get("column_start")),
        json_u32(span.get("line_end")),
        json_u32(span.get("column_end")),
    ))
}

/// Parse TypeScript plain-text diagnostics.
///
/// # Contract
/// - requires: `input` is UTF-8 TypeScript compiler text output.
/// - ensures: returns diagnostics parsed from `path(line,column): severity
///   TScode: message` lines.
/// - fails: returns a parser error when non-empty input contains no TypeScript
///   diagnostics.
/// - panics: none.
fn parse_typescript_text(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_typescript_lines(input.lines().map(Ok::<_, AifixError>))
}

/// Parse TypeScript diagnostics incrementally from a buffered reader.
///
/// # Errors
/// Returns IO errors from the reader or a parser error when non-empty input
/// contains no TypeScript diagnostics.
fn parse_typescript_text_reader<Reader>(reader: Reader) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    parse_typescript_lines(reader.lines().map(|line| line.map_err(AifixError::io)))
}

/// Normalize a fallible sequence of TypeScript diagnostic lines.
///
/// # Errors
/// Returns line-source errors or a parser error when non-empty input contains
/// no TypeScript diagnostics.
fn parse_typescript_lines<Lines, Line>(lines: Lines) -> Result<Vec<Diagnostic>, AifixError>
where
    Lines: IntoIterator<Item = Result<Line, AifixError>>,
    Line: AsRef<str>,
{
    let mut diagnostics = Vec::new();
    let mut saw_non_empty = false;
    for line in lines {
        let line = line?;
        let line = line.as_ref();
        saw_non_empty |= !line.trim().is_empty();
        if let Some(diagnostic) = parse_typescript_line(line) {
            diagnostics.push(diagnostic);
        }
    }

    if diagnostics.is_empty() && saw_non_empty {
        return Err(AifixError::parser(
            "typescript text input did not contain TS diagnostics",
        ));
    }

    debug_assert!(
        !saw_non_empty || !diagnostics.is_empty(),
        "non-empty successful TypeScript parsing must produce diagnostics"
    );
    Ok(diagnostics)
}

/// Parse one `path(line,column): error TS1234: message` line.
///
/// # Contract
/// - requires: `line` is one logical TypeScript diagnostic line.
/// - ensures: returns a normalized `tsc` diagnostic with one span when the line
///   matches.
/// - fails: returns `None` for malformed positions, missing severity, or
///   missing `TS` code/message.
/// - panics: none; all string slicing is checked.
fn parse_typescript_line(line: &str) -> Option<Diagnostic>
{
    let open = line.find('(')?;
    let close = line.get(open ..)?.find("):")? + open;
    let file = non_empty_trimmed_owned(line.get(.. open)?)?;
    let position = line.get(open + 1 .. close)?;
    let rest = line.get(close + 2 ..)?.trim();
    let mut coordinates = position.split(',');
    let line_number = coordinates.next().and_then(parse_u32_str)?;
    let column = coordinates.next().and_then(parse_u32_str)?;
    if coordinates.next().is_some() {
        return None;
    }
    let (severity, after_severity) = split_typescript_severity(rest)?;
    let (code, message) = split_code_message(after_severity)?;
    debug_assert!(
        !file.is_empty(),
        "TypeScript diagnostic file must be non-empty"
    );
    debug_assert!(
        !code.is_empty(),
        "TypeScript diagnostic code must be non-empty"
    );
    debug_assert!(
        !message.trim().is_empty(),
        "TypeScript diagnostic message must be non-empty"
    );
    let span = Span::new(file, Some(line_number), Some(column), None, None);

    Some(
        Diagnostic::new("tsc", Some(code), severity, message).with_details(
            vec![span],
            Vec::new(),
            None,
        ),
    )
}

/// Split the TypeScript severity prefix from a diagnostic line.
fn split_typescript_severity(rest: &str) -> Option<(Severity, &str)>
{
    match rest.strip_prefix("error ") {
        | Some(after) => Some((Severity::Error, after)),
        | None => rest
            .strip_prefix("warning ")
            .map(|after| (Severity::Warning, after)),
    }
}

/// Split a TypeScript code and message body.
fn split_code_message(rest: &str) -> Option<(String, String)>
{
    let delimiter = rest.find(':')?;
    let code = non_empty_trimmed_owned(rest.get(.. delimiter)?)?;
    let message = non_empty_trimmed_owned(rest.get(delimiter + 1 ..)?)?;
    let has_typescript_code = code.starts_with("TS");
    if has_typescript_code {
        debug_assert!(!code.is_empty(), "TypeScript code must be non-empty");
        debug_assert!(
            !message.trim().is_empty(),
            "TypeScript message must be non-empty"
        );
    }
    has_typescript_code.then_some((code, message))
}

/// Probe Agda text diagnostics when at least one Agda-shaped line is present.
///
/// # Contract
/// - requires: `input` is non-empty UTF-8 text.
/// - ensures: returns [`AutoProbe::NoMatch`] unless a complete Agda diagnostic
///   header or recognized Agda progress line can be parsed; valid Agda output
///   is normalized before generic text fallback.
/// - fails: parser failures after an Agda header or progress-line match become
///   [`AutoProbe::Invalid`].
/// - panics: none.
fn probe_agda_text(input: &str) -> AutoProbe
{
    if input
        .lines()
        .any(|line| parse_agda_header(line).is_some() || is_agda_status_line(line))
    {
        AutoProbe::from_result(parse_agda_text(input))
    }
    else {
        AutoProbe::NoMatch
    }
}

/// Parse direct Agda text diagnostics.
///
/// # Contract
/// - requires: `input` is UTF-8 Agda CLI diagnostic text.
/// - ensures: ignores status lines, groups each Agda header with following
///   non-header body lines, and returns `agda` diagnostics with a span and the
///   Agda tag preserved as the diagnostic code. Status-only successful output
///   yields zero diagnostics.
/// - fails: returns a parser error when explicitly selected non-empty input has
///   no Agda-shaped diagnostic headers and includes non-status output.
/// - panics: none; malformed candidate headers are treated as non-matches.
fn parse_agda_text(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_agda_lines(input.lines().map(Ok::<_, AifixError>))
}

/// Parse Agda diagnostics incrementally from a buffered reader.
///
/// # Errors
/// Returns IO errors from the reader or a parser error when non-status input
/// contains no Agda diagnostic header.
fn parse_agda_text_reader<Reader>(reader: Reader) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    parse_agda_lines(reader.lines().map(|line| line.map_err(AifixError::io)))
}

/// Normalize a fallible sequence of Agda output lines.
///
/// # Errors
/// Returns line-source errors or a parser error when non-status input contains
/// no Agda diagnostic header.
fn parse_agda_lines<Lines, Line>(lines: Lines) -> Result<Vec<Diagnostic>, AifixError>
where
    Lines: IntoIterator<Item = Result<Line, AifixError>>,
    Line: AsRef<str>,
{
    let mut diagnostics = Vec::new();
    let mut current: Option<AgdaDiagnosticBuilder> = None;
    let mut saw_non_status_line = false;

    for line in lines {
        let line = line?;
        let line = line.as_ref();
        if is_agda_status_line(line) {
            continue;
        }
        saw_non_status_line |= !line.trim().is_empty();

        if let Some(header) = parse_agda_header(line) {
            if let Some(builder) = current.take() {
                diagnostics.push(builder.finish());
            }
            current = Some(AgdaDiagnosticBuilder::new(header));
        }
        else if let Some(builder) = current.as_mut() {
            builder.push_body_line(line);
        }
    }

    if let Some(builder) = current {
        diagnostics.push(builder.finish());
    }

    if diagnostics.is_empty() && saw_non_status_line {
        return Err(AifixError::parser(
            "agda text input did not contain Agda diagnostics",
        ));
    }

    debug_assert!(
        !diagnostics.is_empty() || !saw_non_status_line,
        "non-empty successful Agda parsing must produce diagnostics or contain only status lines"
    );
    Ok(diagnostics)
}

/// Parsed fields from one Agda diagnostic header.
struct AgdaHeader
{
    /// File path reported before the one-based source position.
    file: String,
    /// One-based source start line.
    line: u32,
    /// One-based inclusive start column.
    start_column: u32,
    /// One-based source end line.
    end_line: u32,
    /// One-based inclusive end column.
    end_column: u32,
    /// Severity label reported by Agda.
    severity: Severity,
    /// Agda diagnostic tag without surrounding brackets.
    tag: String,
}

/// Accumulate body lines for one Agda diagnostic before model construction.
struct AgdaDiagnosticBuilder
{
    /// Header fields shared by the finished diagnostic.
    header: AgdaHeader,
    /// Body lines following the header until the next header.
    body: Vec<String>,
}

impl AgdaDiagnosticBuilder
{
    /// Start a diagnostic from an already parsed Agda header.
    ///
    /// # Contract
    /// - requires: `header` was parsed from a complete Agda header line.
    /// - ensures: returns a builder with no body lines yet.
    /// - panics: none.
    fn new(header: AgdaHeader) -> Self
    {
        Self {
            header,
            body: Vec::new(),
        }
    }

    /// Add one body line exactly as Agda emitted it.
    ///
    /// # Contract
    /// - requires: `line` is a non-header diagnostic body line.
    /// - ensures: preserves body text for later surrounding-whitespace trim.
    /// - panics: none.
    fn push_body_line(
        &mut self,
        line: &str,
    )
    {
        self.body.push(line.to_owned());
    }

    /// Finish a normalized Agda diagnostic.
    ///
    /// # Contract
    /// - requires: the builder contains a valid header.
    /// - ensures: returns a diagnostic with source `agda`, the header tag as
    ///   code, body text trimmed around the whole message, and the tag as a
    ///   fallback message when the body is blank.
    /// - panics: none.
    fn finish(self) -> Diagnostic
    {
        let message = non_empty_trimmed_owned(&self.body.join("\n"))
            .unwrap_or_else(|| self.header.tag.clone());
        let span = Span::new(
            self.header.file,
            Some(self.header.line),
            Some(self.header.start_column),
            Some(self.header.end_line),
            Some(self.header.end_column),
        );

        Diagnostic::new("agda", Some(self.header.tag), self.header.severity, message).with_details(
            vec![span],
            Vec::new(),
            None,
        )
    }
}

/// Parse one Agda header line.
///
/// # Contract
/// - requires: `line` is one logical line of Agda CLI output.
/// - ensures: returns parsed header fields only for `file:line.start-end:
///   severity: [tag]` and `file:start-line.start-column-end-line.end-column:
///   severity: [tag]` lines.
/// - fails: returns `None` for missing or invalid captures.
/// - panics: none; all string slicing uses checked split operations.
fn parse_agda_header(line: &str) -> Option<AgdaHeader>
{
    let line = line.trim_end();
    let (before_tag, tag_with_close) = line.rsplit_once(": [")?;
    let tag = non_empty_trimmed_owned(tag_with_close.strip_suffix(']')?)?;
    let (location, severity_text) = before_tag.rsplit_once(':')?;
    let severity_text = severity_text.trim();
    if severity_text.is_empty() {
        return None;
    }
    let (file, position_range) = location.rsplit_once(':')?;
    let (start_position, end_position) = position_range.split_once('-')?;
    let (line_number, start_column) = parse_agda_position(start_position)?;
    let (end_line, end_column) = if end_position.contains('.') {
        parse_agda_position(end_position)?
    }
    else {
        (line_number, parse_u32_str(end_position)?)
    };
    let file = non_empty_trimmed_owned(file)?;
    if line_number == 0 || start_column == 0 || end_line == 0 || end_column == 0 {
        return None;
    }
    if end_line < line_number || (end_line == line_number && start_column > end_column) {
        return None;
    }

    Some(AgdaHeader {
        file,
        line: line_number,
        start_column,
        end_line,
        end_column,
        severity: line_severity(severity_text),
        tag,
    })
}

/// Parse a one-based Agda `line.column` position pair.
///
/// # Contract
/// - requires: `position` is a candidate Agda source position.
/// - ensures: returns parsed line and column when both are unsigned integers.
/// - fails: returns `None` for missing or invalid captures.
/// - panics: none.
fn parse_agda_position(position: &str) -> Option<(u32, u32)>
{
    let (line, column) = position.split_once('.')?;
    Some((parse_u32_str(line)?, parse_u32_str(column)?))
}

/// Return whether an Agda line is progress/status output rather than a body.
///
/// # Contract
/// - requires: `line` is one logical Agda output line.
/// - ensures: recognizes exact progress lines that should not become
///   diagnostics or diagnostic messages.
/// - panics: none.
fn is_agda_status_line(line: &str) -> bool
{
    is_agda_checking_line(line) || is_agda_finished_line(line)
}

/// Return whether a line has Agda's `Checking <module> (<path>).` shape.
///
/// # Contract
/// - requires: `line` is one logical Agda output line.
/// - ensures: returns true only when both module and path fields are non-empty.
/// - panics: none; all slicing uses checked split operations.
fn is_agda_checking_line(line: &str) -> bool
{
    let Some(rest) = line.trim_end().strip_prefix("Checking ")
    else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(").")
    else {
        return false;
    };
    let Some((module, path)) = rest.split_once(" (")
    else {
        return false;
    };
    !module.trim().is_empty() && !path.trim().is_empty()
}

/// Return whether a line has Agda's `Finished <module>.` shape.
///
/// # Contract
/// - requires: `line` is one logical Agda output line.
/// - ensures: returns true only when the module field is non-empty.
/// - panics: none.
fn is_agda_finished_line(line: &str) -> bool
{
    let Some(module) = line.trim_end().strip_prefix("Finished ")
    else {
        return false;
    };
    let Some(module) = module.strip_suffix('.')
    else {
        return false;
    };
    !module.trim().is_empty()
}

/// Parse LSP diagnostic arrays, wrapper objects, or publishDiagnostics params.
///
/// # Contract
/// - requires: `input` is JSON containing an array, `diagnostics`, or
///   `params.diagnostics`.
/// - ensures: returns normalized diagnostics and uses wrapper URI as a fallback
///   span file.
/// - fails: returns JSON errors for malformed JSON or parser errors for
///   missing/non-array diagnostics.
/// - panics: none.
fn parse_lsp_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_lsp_value(&parse_json_value(input)?)
}

/// Convert parsed LSP JSON into normalized diagnostics.
///
/// # Contract
/// - requires: `value` contains an LSP diagnostic array, wrapper object, or
///   publishDiagnostics params object.
/// - ensures: returns normalized diagnostics and uses wrapper URI as a fallback
///   span file.
/// - fails: returns parser errors for missing/non-array diagnostics, blank
///   messages, malformed ranges, or reversed ranges.
/// - panics: none.
pub(crate) fn parse_lsp_value(value: &Value) -> Result<Vec<Diagnostic>, AifixError>
{
    let fallback_uri = lsp_fallback_uri(value);
    let diagnostics_value = lsp_diagnostics_value(value)?;
    let array = diagnostics_value
        .as_array()
        .ok_or_else(|| AifixError::parser("LSP diagnostics was not an array"))?;

    let diagnostics = array
        .iter()
        .map(|diagnostic| lsp_diagnostic(diagnostic, fallback_uri.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    debug_assert!(
        diagnostics.len() == array.len(),
        "LSP normalization should validate every input diagnostic"
    );
    Ok(diagnostics)
}

/// Return the diagnostics member from a parsed LSP payload.
///
/// # Contract
/// - requires: `value` is parsed JSON from an LSP adapter boundary.
/// - ensures: returns the array candidate from an array, `diagnostics`, or
///   `params.diagnostics` shape without cloning it.
/// - fails: returns a parser error when no LSP diagnostics field is present.
/// - panics: none.
fn lsp_diagnostics_value(value: &Value) -> Result<&Value, AifixError>
{
    if value.is_array() {
        Ok(value)
    }
    else if let Some(diagnostics) = value.get("diagnostics") {
        Ok(diagnostics)
    }
    else if let Some(params) = value.get("params") {
        params
            .get("diagnostics")
            .ok_or_else(|| AifixError::parser("LSP JSON params did not contain diagnostics"))
    }
    else {
        Err(AifixError::parser("LSP JSON did not contain diagnostics"))
    }
}

/// Return the URI used for LSP span file fallback.
fn lsp_fallback_uri(value: &Value) -> Option<String>
{
    value
        .get("uri")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("params")
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
}

/// Convert one LSP diagnostic object into a normalized diagnostic.
///
/// # Contract
/// - requires: `value` is an LSP diagnostic object and `fallback_uri` is the
///   enclosing URI when present.
/// - ensures: returns a diagnostic with preserved raw JSON and one validated
///   normalized span.
/// - fails: returns a parser error when message text is absent, blank, or when
///   range validation fails.
/// - panics: none.
fn lsp_diagnostic(
    value: &Value,
    fallback_uri: Option<&str>,
) -> Result<Diagnostic, AifixError>
{
    let message = first_non_empty_string(value, &["message"])
        .ok_or_else(|| AifixError::parser("LSP diagnostic missing non-empty message"))?;
    debug_assert!(
        !message.trim().is_empty(),
        "LSP diagnostic messages should be non-empty"
    );
    let source = match first_non_empty_string(value, &["source"]) {
        | Some(source) => source,
        | None => "lsp".to_owned(),
    };
    debug_assert!(
        !source.trim().is_empty(),
        "LSP diagnostic source must be non-empty"
    );
    let code = value.get("code").and_then(|candidate| {
        candidate
            .as_str()
            .and_then(non_empty_trimmed_owned)
            .or_else(|| candidate.as_number().map(ToString::to_string))
    });
    let severity = value
        .get("severity")
        .and_then(Value::as_u64)
        .map_or(Severity::Warning, lsp_severity);
    let spans = vec![lsp_span(value.get("range"), fallback_uri)?];

    Ok(
        Diagnostic::new(source, code, severity, message).with_details(
            spans,
            Vec::new(),
            Some(value.clone()),
        ),
    )
}

/// Convert LSP numeric severity into normalized severity.
fn lsp_severity(value: u64) -> Severity
{
    match value {
        | 1 => Severity::Error,
        | 2 => Severity::Warning,
        | 4 => Severity::Hint,
        | _ => Severity::Info,
    }
}

/// Convert an LSP zero-based range and optional URI into a normalized span.
///
/// # Contract
/// - requires: `range` follows the LSP shape with a `start` object; coordinates
///   are zero-based.
/// - ensures: returns a span with validated one-based coordinates and an empty
///   file only when no URI is available.
/// - fails: returns a parser error when range objects, coordinates, one-based
///   conversion, or ordering are invalid.
/// - panics: none.
fn lsp_span(
    range: Option<&Value>,
    uri: Option<&str>,
) -> Result<Span, AifixError>
{
    let range = range.ok_or_else(|| AifixError::parser("LSP diagnostic missing range"))?;
    let start = range
        .get("start")
        .ok_or_else(|| AifixError::parser("LSP range missing start"))?;
    let end = range
        .get("end")
        .ok_or_else(|| AifixError::parser("LSP range missing end"))?;
    let start_line = json_u32(start.get("line"))
        .ok_or_else(|| AifixError::parser("LSP range start missing line"))?;
    let start_character = json_u32(start.get("character"))
        .ok_or_else(|| AifixError::parser("LSP range start missing character"))?;
    let end_line = json_u32(end.get("line"))
        .ok_or_else(|| AifixError::parser("LSP range end missing line"))?;
    let end_character = json_u32(end.get("character"))
        .ok_or_else(|| AifixError::parser("LSP range end missing character"))?;
    if start_line > end_line || (start_line == end_line && start_character > end_character) {
        return Err(AifixError::parser("LSP range start must not be after end"));
    }
    let start_line = one_based(start_line)
        .ok_or_else(|| AifixError::parser("LSP range start line overflowed"))?;
    let start_character = one_based(start_character)
        .ok_or_else(|| AifixError::parser("LSP range start character overflowed"))?;
    let end_line =
        one_based(end_line).ok_or_else(|| AifixError::parser("LSP range end line overflowed"))?;
    let end_character = one_based(end_character)
        .ok_or_else(|| AifixError::parser("LSP range end character overflowed"))?;
    let span = Span::new(
        uri.map_or_else(String::new, str::to_owned),
        Some(start_line),
        Some(start_character),
        Some(end_line),
        Some(end_character),
    );
    debug_assert!(
        span.line <= span.end_line,
        "validated LSP span start line must not exceed end line"
    );
    Ok(span)
}

/// Parse Nushell linter text, or generic non-empty diagnostic lines.
///
/// # Contract
/// - requires: `input` is UTF-8 diagnostic text with one diagnostic per
///   non-empty line.
/// - ensures: returns one normalized Nushell diagnostic per non-empty trimmed
///   line.
/// - fails: returns a parser error only when non-empty input yields no
///   diagnostics.
/// - panics: none.
fn parse_nushell_text(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_nushell_lines(input.lines().map(Ok::<_, AifixError>))
}

/// Parse generic diagnostics incrementally from a buffered reader.
///
/// # Errors
/// Returns IO errors from the reader or a parser error when non-empty input
/// yields no diagnostics.
fn parse_nushell_text_reader<Reader>(reader: Reader) -> Result<Vec<Diagnostic>, AifixError>
where
    Reader: BufRead,
{
    parse_nushell_lines(reader.lines().map(|line| line.map_err(AifixError::io)))
}

/// Normalize a fallible sequence of generic diagnostic lines.
///
/// # Errors
/// Returns line-source errors or a parser error when non-empty input yields no
/// diagnostics.
fn parse_nushell_lines<Lines, Line>(lines: Lines) -> Result<Vec<Diagnostic>, AifixError>
where
    Lines: IntoIterator<Item = Result<Line, AifixError>>,
    Line: AsRef<str>,
{
    let mut diagnostics = Vec::new();
    let mut saw_non_empty = false;
    for line in lines {
        let line = line?;
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        saw_non_empty = true;
        diagnostics.push(Diagnostic::new(
            "nushell",
            None,
            line_severity(line),
            line.to_owned(),
        ));
    }

    if diagnostics.is_empty() && saw_non_empty {
        return Err(AifixError::parser(
            "nushell text input did not contain diagnostics",
        ));
    }

    debug_assert!(
        !saw_non_empty || !diagnostics.is_empty(),
        "non-empty successful Nushell parsing must produce diagnostics"
    );
    Ok(diagnostics)
}

/// Infer severity from a generic diagnostic line.
fn line_severity(line: &str) -> Severity
{
    debug_assert!(
        !line.trim().is_empty(),
        "generic line severity inference expects non-empty lines"
    );
    let lower = line.to_ascii_lowercase();
    if lower.contains("error") {
        Severity::Error
    }
    else if lower.contains("warn") {
        Severity::Warning
    }
    else {
        Severity::Info
    }
}

/// Return whether a line has cargo JSON shape markers.
///
/// # Contract
/// - requires: `line` is one logical input line.
/// - ensures: returns true only for non-empty lines that look like JSON objects
///   and mention cargo's `reason` or `compiler-message` fields; malformed JSON
///   can still return true so the cargo parser reports the real structured
///   error.
/// - panics: none.
fn is_cargo_json_line(line: &str) -> bool
{
    let trimmed = line.trim_start();
    trimmed.starts_with('{')
        && (trimmed.contains("\"reason\"") || trimmed.contains("\"compiler-message\""))
}

/// Return whether input starts with a complete-JSON delimiter.
fn looks_like_complete_json(input: &str) -> bool
{
    matches!(input.trim_start().as_bytes().first(), Some(b'{' | b'['))
}

/// Return whether parsed JSON has an LSP diagnostics shape.
fn lsp_json_shape(value: &Value) -> bool
{
    if value.is_array() {
        return value
            .as_array()
            .is_some_and(|items| items.iter().any(lsp_diagnostic_shape));
    }
    if value
        .get("params")
        .and_then(|params| params.get("diagnostics"))
        .is_some()
    {
        return true;
    }
    if value.get("uri").is_some() && value.get("diagnostics").is_some() {
        return true;
    }
    value.get("diagnostics").is_some_and(lsp_diagnostics_shape)
}

/// Return whether a value looks like an LSP diagnostics array.
fn lsp_diagnostics_shape(value: &Value) -> bool
{
    value
        .as_array()
        .is_some_and(|items| items.iter().any(lsp_diagnostic_shape))
}

/// Return whether a value looks like one LSP diagnostic object.
fn lsp_diagnostic_shape(value: &Value) -> bool
{
    value.get("range").is_some()
}

/// Parse a complete JSON value from input.
///
/// # Contract
/// - requires: `input` is intended to contain one complete JSON value.
/// - ensures: returns the parsed [`Value`] without modifying source text.
/// - fails: returns [`AifixError::Json`] for malformed or incomplete JSON.
/// - panics: none.
fn parse_json_value(input: &str) -> Result<Value, AifixError>
{
    serde_json::from_str(input).map_err(AifixError::Json)
}

/// Return the first string property found on a JSON object.
fn first_string(
    value: &Value,
    keys: &[&str],
) -> Option<String>
{
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

/// Return the first non-empty string property found on a JSON object.
fn first_non_empty_string(
    value: &Value,
    keys: &[&str],
) -> Option<String>
{
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .and_then(non_empty_trimmed_owned)
}

/// Return trimmed owned text when it is not empty.
fn non_empty_trimmed_owned(value: &str) -> Option<String>
{
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Convert a JSON number to `u32` without panicking.
fn json_u32(value: Option<&Value>) -> Option<u32>
{
    let number = value?.as_u64()?;
    u32::try_from(number).ok()
}

/// Parse a decimal `u32` from text.
fn parse_u32_str(value: &str) -> Option<u32>
{
    value.trim().parse::<u32>().ok()
}

/// Convert a zero-based coordinate into a one-based coordinate.
///
/// # Contract
/// - requires: `value` is a zero-based coordinate from an external protocol.
/// - ensures: returns `value + 1` when representable.
/// - fails: returns `None` on `u32::MAX` overflow instead of panicking.
/// - panics: none.
fn one_based(value: u32) -> Option<u32>
{
    value.checked_add(1)
}

/// Unit tests for adapter shape detection and runtime field validation.
#[cfg(test)]
mod tests
{
    use super::parse_clippy_json;
    use super::parse_diagnostics;
    use crate::error::AifixError;
    use crate::model::Protocol;

    /// Require a condition in adapter unit tests without panicking directly.
    fn require(
        condition: bool,
        message: &str,
    ) -> Result<(), AifixError>
    {
        if condition {
            Ok(())
        }
        else {
            Err(AifixError::parser(message))
        }
    }

    /// Auto mode rejects malformed structured JSON instead of generic fallback.
    #[test]
    fn auto_rejects_malformed_structured_json() -> Result<(), AifixError>
    {
        let error = parse_diagnostics(
            Protocol::Auto,
            r#"{"diagnostics":[{"range":{"start":{"line":1,"character":1},"end":{"line":1,"character":2}},"message":" "}]} "#,
        )
        .err()
        .ok_or_else(|| AifixError::parser("auto mode accepted malformed LSP JSON"))?;

        require(
            matches!(error, AifixError::Parser(_)),
            "auto mode should return a structured parser error",
        )
    }

    /// Explicit Clippy parsing accepts completed streams with no diagnostics.
    #[test]
    fn clippy_json_accepts_clean_output() -> Result<(), AifixError>
    {
        for input in [
            "",
            " \n\t\n",
            concat!(
                "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///demo#0.1.0\",\"manifest_path\":\"/demo/Cargo.toml\",\"target\":{\"kind\":[\"bin\"],\"crate_types\":[\"bin\"],\"name\":\"demo\",\"src_path\":\"/demo/src/main.rs\",\"edition\":\"2024\",\"doc\":true,\"doctest\":false,\"test\":true},\"profile\":{\"opt_level\":\"0\",\"debuginfo\":\"line-tables-only\",\"debug_assertions\":true,\"overflow_checks\":true,\"test\":false},\"features\":[],\"filenames\":[\"/demo/target/debug/demo\"],\"executable\":\"/demo/target/debug/demo\",\"fresh\":true}\n",
                "Finished `dev` profile [unoptimized + debuginfo] target(s)\n",
            ),
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        ] {
            let diagnostics = parse_clippy_json(input)?;
            require(
                diagnostics.is_empty(),
                "clean Clippy JSON should contain no diagnostics",
            )?;
        }
        Ok(())
    }

    /// Explicit Clippy parsing still rejects malformed structured output.
    #[test]
    fn clippy_json_rejects_malformed_output() -> Result<(), AifixError>
    {
        let error = parse_clippy_json(
            "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"truncated\"",
        )
        .err()
        .ok_or_else(|| AifixError::parser("explicit Clippy parsing accepted malformed JSON"))?;

        require(
            matches!(error, AifixError::Json(_)),
            "malformed Clippy JSON should return a JSON error",
        )
    }

    /// Explicit Clippy parsing rejects valid JSON without a Cargo event shape.
    #[test]
    fn clippy_json_rejects_malformed_cargo_records() -> Result<(), AifixError>
    {
        for input in [
            "null\n",
            "{}\n",
            "[]\n",
            "{\"reason\":5}\n",
            "{\"reason\":\"compiler-artifact\"}\n",
        ] {
            let error = parse_clippy_json(input)
                .err()
                .ok_or_else(|| AifixError::parser("accepted malformed Cargo JSON record"))?;
            require(
                matches!(error, AifixError::Parser(_)),
                "malformed Cargo JSON should return a parser error",
            )?;
        }
        Ok(())
    }

    /// Explicit Clippy parsing rejects non-empty streams without Cargo JSON.
    #[test]
    fn clippy_json_rejects_plain_text_output() -> Result<(), AifixError>
    {
        let error = parse_clippy_json("Finished `dev` profile\n")
            .err()
            .ok_or_else(|| AifixError::parser("explicit Clippy parsing accepted plain text"))?;

        require(
            matches!(error, AifixError::Parser(_)),
            "plain text under the Clippy protocol should return a parser error",
        )
    }

    /// Explicit Clippy parsing retains ordinary compiler diagnostics.
    #[test]
    fn clippy_json_retains_compiler_messages() -> Result<(), AifixError>
    {
        let diagnostics = parse_clippy_json(
            "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"used `unwrap()` on an `Option` value\",\"level\":\"warning\",\"code\":{\"code\":\"clippy::unwrap_used\"},\"spans\":[]}}\n",
        )?;
        let diagnostic = diagnostics
            .first()
            .ok_or_else(|| AifixError::parser("explicit Clippy parsing returned no diagnostic"))?;

        require(
            diagnostics.len() == 1
                && diagnostic.code.as_deref() == Some("clippy::unwrap_used")
                && diagnostic.source == "clippy",
            "explicit Clippy parsing should retain compiler-message diagnostics",
        )
    }

    /// Cargo auto parsing keeps valid compiler diagnostics beside noisy lines.
    #[test]
    fn auto_cargo_stream_retains_diagnostics_with_noise() -> Result<(), AifixError>
    {
        let diagnostics = parse_diagnostics(
            Protocol::Auto,
            concat!(
                "{\"reason\":\"compiler-artifact\",\"package_id\":\"demo 0.1.0\"}\n",
                "warning: build script printed a non-json line\n",
                "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"used `unwrap()` on an `Option` value\",\"level\":\"warning\",\"code\":{\"code\":\"clippy::unwrap_used\"},\"spans\":[{\"file_name\":\"src/main.rs\",\"line_start\":7,\"column_start\":9,\"line_end\":7,\"column_end\":15,\"is_primary\":true}]}}\n",
                "{\"reason\":\"compiler-message\",\"message\":"
            ),
        )?;

        require(
            diagnostics.len() == 1,
            "noisy cargo stream should keep one diagnostic",
        )?;
        let diagnostic = diagnostics
            .first()
            .ok_or_else(|| AifixError::parser("noisy cargo stream returned no diagnostics"))?;
        require(
            diagnostic.code.as_deref() == Some("clippy::unwrap_used"),
            "cargo diagnostic should preserve the clippy code",
        )?;
        require(
            diagnostic.source == "clippy",
            "cargo diagnostic should be classified as clippy",
        )
    }

    /// Cargo auto parsing rejects malformed structured streams with no payload.
    #[test]
    fn auto_cargo_malformed_only_is_rejected() -> Result<(), AifixError>
    {
        let error = parse_diagnostics(
            Protocol::Auto,
            "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"truncated\"",
        )
        .err()
        .ok_or_else(|| AifixError::parser("auto mode accepted malformed cargo JSON"))?;

        require(
            matches!(error, AifixError::Json(_)),
            "malformed-only cargo stream should return a JSON error",
        )
    }

    /// LSP normalization falls back from blank source to `lsp`.
    #[test]
    fn lsp_blank_source_defaults_to_lsp() -> Result<(), AifixError>
    {
        let mut diagnostics = parse_diagnostics(
            Protocol::LspJson,
            r#"{"uri":"file:///tmp/app.ts","diagnostics":[{"range":{"start":{"line":1,"character":1},"end":{"line":1,"character":2}},"source":" ","message":"Cannot find name"}]}"#,
        )?
        .into_iter();
        let diagnostic = diagnostics
            .next()
            .ok_or_else(|| AifixError::parser("LSP parser returned no diagnostics"))?;

        require(
            diagnostic.source == "lsp",
            "blank LSP source should fall back to lsp",
        )
    }

    /// LSP normalization rejects reversed ranges before constructing spans.
    #[test]
    fn lsp_rejects_reversed_ranges() -> Result<(), AifixError>
    {
        let error = parse_diagnostics(
            Protocol::LspJson,
            r#"{"diagnostics":[{"range":{"start":{"line":3,"character":4},"end":{"line":3,"character":2}},"message":"bad range"}]}"#,
        )
        .err()
        .ok_or_else(|| AifixError::parser("LSP parser accepted a reversed range"))?;

        require(
            matches!(error, AifixError::Parser(_)),
            "reversed LSP ranges should return parser errors",
        )
    }

    /// Protocol parsing accepts the stable Agda spelling and documented
    /// aliases.
    #[test]
    fn protocol_parses_agda_text_aliases() -> Result<(), AifixError>
    {
        for alias in ["agda-text", "agda_text", "agda", "agda-cli"] {
            require(
                alias.parse::<Protocol>()? == Protocol::AgdaText,
                "Agda protocol alias should resolve to AgdaText",
            )?;
        }

        Ok(())
    }

    /// Agda text parsing preserves `UnequalSorts` fields and message body.
    #[test]
    fn agda_text_parses_unequal_sorts() -> Result<(), AifixError>
    {
        let mut diagnostics = parse_diagnostics(
            Protocol::AgdaText,
            concat!(
                "Checking Bad (/private/tmp/aifix-agda-smoke/Bad.agda).\n",
                "/private/tmp/aifix-agda-smoke/Bad.agda:4.7-10: error: [UnequalSorts]\n",
                "Set₁ != Set\n",
                "when checking that the expression Set has type Set\n",
            ),
        )?
        .into_iter();
        let diagnostic = diagnostics
            .next()
            .ok_or_else(|| AifixError::parser("Agda parser returned no diagnostics"))?;
        require(diagnostic.source == "agda", "Agda source should be agda")?;
        require(
            diagnostic.code.as_deref() == Some("UnequalSorts"),
            "Agda code should preserve UnequalSorts",
        )?;
        require(
            diagnostic.message
                == concat!(
                    "Set₁ != Set\n",
                    "when checking that the expression Set has type Set",
                ),
            "Agda body should preserve multiline message text",
        )?;
        let span = diagnostic
            .spans
            .first()
            .ok_or_else(|| AifixError::parser("Agda diagnostic had no span"))?;
        require(
            span.file == "/private/tmp/aifix-agda-smoke/Bad.agda",
            "Agda span should preserve file",
        )?;
        require(span.line == Some(4), "Agda span should preserve line")?;
        require(
            span.column == Some(7),
            "Agda span should preserve start column",
        )?;
        require(
            span.end_line == Some(4),
            "Agda span should use same-line end line",
        )?;
        require(
            span.end_column == Some(10),
            "Agda span should preserve end column",
        )?;
        require(
            diagnostic.raw.is_none(),
            "Agda text should not preserve raw JSON",
        )?;
        Ok(())
    }

    /// Agda text parsing groups `FileNotFound` multiline bodies.
    #[test]
    fn agda_text_parses_file_not_found_multiline_body() -> Result<(), AifixError>
    {
        let mut diagnostics = parse_diagnostics(
            Protocol::AgdaText,
            concat!(
                "/private/tmp/aifix-agda-smoke/Missing.agda:3.1-25: error: [FileNotFound]\n",
                "Failed to find source of module DoesNotExist ...\n",
                "when scope checking the declaration\n",
                "  open import DoesNotExist\n",
            ),
        )?
        .into_iter();
        let diagnostic = diagnostics
            .next()
            .ok_or_else(|| AifixError::parser("Agda parser returned no diagnostics"))?;
        require(
            diagnostic.code.as_deref() == Some("FileNotFound"),
            "Agda code should preserve FileNotFound",
        )?;
        require(
            diagnostic.message
                == concat!(
                    "Failed to find source of module DoesNotExist ...\n",
                    "when scope checking the declaration\n",
                    "  open import DoesNotExist",
                ),
            "Agda FileNotFound body should include all non-header lines",
        )?;
        let span = diagnostic
            .spans
            .first()
            .ok_or_else(|| AifixError::parser("Agda diagnostic had no span"))?;
        require(span.line == Some(3), "FileNotFound line should be parsed")?;
        require(
            span.column == Some(1),
            "FileNotFound start column should be parsed",
        )?;
        require(
            span.end_column == Some(25),
            "FileNotFound end column should be parsed",
        )
    }

    /// Agda text parsing preserves `UnsolvedInteractionMetas` diagnostics.
    #[test]
    fn agda_text_parses_unsolved_interaction_metas() -> Result<(), AifixError>
    {
        let mut diagnostics = parse_diagnostics(
            Protocol::AgdaText,
            concat!(
                "/private/tmp/aifix-agda-smoke/Hole.agda:6.5-6: error: ",
                "[UnsolvedInteractionMetas]\n",
                "Unsolved interaction metas at the following locations:\n",
                "  /private/tmp/aifix-agda-smoke/Hole.agda:6,5-6\n",
            ),
        )?
        .into_iter();
        let diagnostic = diagnostics
            .next()
            .ok_or_else(|| AifixError::parser("Agda parser returned no diagnostics"))?;
        require(
            diagnostic.code.as_deref() == Some("UnsolvedInteractionMetas"),
            "Agda code should preserve UnsolvedInteractionMetas",
        )?;
        require(
            diagnostic.message.contains("Unsolved interaction metas"),
            "Agda UnsolvedInteractionMetas message should preserve body",
        )?;
        let span = diagnostic
            .spans
            .first()
            .ok_or_else(|| AifixError::parser("Agda diagnostic had no span"))?;
        require(span.line == Some(6), "hole span line should be parsed")?;
        require(
            span.column == Some(5),
            "hole span start column should be parsed",
        )?;
        require(
            span.end_column == Some(6),
            "hole span end column should be parsed",
        )
    }

    /// Auto mode detects Agda text before falling back to generic text.
    #[test]
    fn auto_detects_agda_text_before_generic_fallback() -> Result<(), AifixError>
    {
        let mut diagnostics = parse_diagnostics(
            Protocol::Auto,
            concat!(
                "Checking Bad (/private/tmp/aifix-agda-smoke/Bad.agda).\n",
                "/private/tmp/aifix-agda-smoke/Bad.agda:4.7-10: error: [UnequalSorts]\n",
                "Set₁ != Set\n",
            ),
        )?
        .into_iter();
        let diagnostic = diagnostics
            .next()
            .ok_or_else(|| AifixError::parser("auto Agda parser returned no diagnostics"))?;
        require(
            diagnostic.source == "agda",
            "auto should select Agda parser instead of generic text",
        )?;
        require(
            diagnostic.code.as_deref() == Some("UnequalSorts"),
            "auto Agda parser should preserve tag code",
        )
    }

    /// TypeScript parsing rejects empty external fields before model creation.
    #[test]
    fn typescript_rejects_empty_external_fields() -> Result<(), AifixError>
    {
        for input in [
            "(1,1): error TS1234: message",
            "src/app.ts(1,1): error : message",
            "src/app.ts(1,1): error TS1234: ",
        ] {
            let error = parse_diagnostics(Protocol::TypescriptText, input)
                .err()
                .ok_or_else(|| AifixError::parser("TypeScript parser accepted an empty field"))?;
            require(
                matches!(error, AifixError::Parser(_)),
                "empty TypeScript fields should return parser errors",
            )?;
        }

        Ok(())
    }
}
