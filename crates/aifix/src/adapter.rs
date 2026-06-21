//! Diagnostic protocol adapters.
//!
//! Adapters translate source-tool output into [`crate::model::Diagnostic`]
//! values without invoking tools or reaching the network.  Structured JSON
//! adapters preserve their raw JSON payload so downstream agents can inspect
//! source-specific details when the normalized fields are insufficient.

use serde_json::Value;

use crate::error::AifixError;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Protocol;
use crate::model::Severity;
use crate::model::Span;
use crate::model::Suggestion;

/// Result of probing one adapter while inferring the diagnostic protocol.
///
/// # Contract
/// - Preconditions: variants are produced only by shape-specific probe helpers.
/// - Postconditions: distinguishes absent protocol shapes from malformed
///   matched protocol input.
/// - Failure modes: [`AutoProbe::Invalid`] carries the parser or JSON boundary
///   that rejected the matched shape.
/// - Panics: none.
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
    /// - Preconditions: the caller has already established that the adapter
    ///   shape is present.
    /// - Postconditions: preserves successful diagnostics and parser errors
    ///   without falling through to generic text parsing.
    /// - Failure modes: returns [`AutoProbe::Invalid`] when `result` is an
    ///   error.
    /// - Panics: none.
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
/// # Errors
/// Returns [`AifixError::Json`] when a selected JSON protocol receives
/// malformed JSON. Returns [`AifixError::Parser`] when non-empty input cannot
/// be normalized by the selected protocol.
///
/// # Contract
/// - Preconditions: `input` is UTF-8 output from the selected diagnostic
///   protocol.
/// - Postconditions: returns deterministic normalized diagnostics without
///   invoking tools or network access.
/// - Failure modes: returns [`AifixError::Json`] for malformed JSON protocols
///   or [`AifixError::Parser`] when non-empty input contains no diagnostics.
/// - Panics: none.
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
        | Protocol::TypescriptText => parse_typescript_text(input),
        | Protocol::LspJson => parse_lsp_json(input),
        | Protocol::NushellText => parse_nushell_text(input),
    }
}

/// Try adapters in an order that rejects malformed structured shapes.
///
/// # Contract
/// - Preconditions: `input` is UTF-8 diagnostic output and may be empty.
/// - Postconditions: empty or whitespace-only input yields an empty vector;
///   structured-looking inputs are handled by their matched adapter, while
///   unstructured text falls back to TypeScript then generic diagnostics.
/// - Failure modes: returns the matched structured parser error instead of
///   silently converting malformed JSON, LSP, or cargo shapes to generic text.
/// - Panics: none.
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

    if let Ok(diagnostics) = parse_typescript_text(input) {
        return Ok(diagnostics);
    }

    parse_nushell_text(input)
}

/// Probe cargo newline-delimited JSON when cargo-shaped fields are present.
///
/// # Contract
/// - Preconditions: `input` is non-empty UTF-8 text.
/// - Postconditions: returns [`AutoProbe::NoMatch`] unless a cargo
///   compiler-message shape marker is present.
/// - Failure modes: malformed cargo-shaped JSON becomes [`AutoProbe::Invalid`]
///   so auto mode cannot fall through to generic lines.
/// - Panics: none.
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
/// - Preconditions: `input` is non-empty UTF-8 text.
/// - Postconditions: returns [`AutoProbe::NoMatch`] for non-JSON-looking text;
///   otherwise a supported structured match or an invalid structured error.
/// - Failure modes: malformed or unsupported JSON-looking diagnostics become
///   [`AutoProbe::Invalid`] instead of generic text diagnostics.
/// - Panics: none.
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
/// - Preconditions: `input` contains a JSON value produced by `aifix` or
///   matching its normalized model.
/// - Postconditions: returns diagnostics from a digest, diagnostic array,
///   single diagnostic, or diagnostics property.
/// - Failure modes: returns JSON errors for malformed JSON or parser errors
///   when no diagnostics shape is present.
/// - Panics: none.
fn parse_aifix_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_aifix_value(&parse_json_value(input)?)
}

/// Convert native normalized `aifix` JSON into diagnostics.
///
/// # Contract
/// - Preconditions: `value` is a parsed JSON value produced by `aifix` or
///   matching its normalized model.
/// - Postconditions: returns diagnostics from a digest, diagnostic array,
///   single diagnostic, or diagnostics property.
/// - Failure modes: returns JSON errors for malformed model-shaped values or a
///   parser error when no diagnostics shape is present.
/// - Panics: none.
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

/// Parse newline-delimited cargo compiler-message JSON.
///
/// # Contract
/// - Preconditions: non-empty lines may contain cargo JSON events, unrelated
///   cargo output, or truncated/noisy stream fragments.
/// - Postconditions: returns normalized compiler-message diagnostics, ignores
///   other cargo message reasons, and retains valid diagnostics even when
///   adjacent lines are malformed.
/// - Failure modes: returns the first JSON/parser error when no valid
///   compiler-message diagnostic can be recovered, or a parser error when
///   non-empty input contains no JSON messages.
/// - Panics: none.
fn parse_clippy_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    let mut diagnostics = Vec::new();
    let mut saw_json = false;
    let mut saw_compiler_message = false;
    let mut first_structured_error = None;

    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let value = match serde_json::from_str::<Value>(line) {
            | Ok(value) => value,
            | Err(error) => {
                if first_structured_error.is_none() {
                    first_structured_error = Some(AifixError::Json(error));
                }
                continue;
            },
        };
        saw_json = true;
        if value.get("reason").and_then(Value::as_str) != Some("compiler-message") {
            continue;
        }
        saw_compiler_message = true;
        let Some(message) = value.get("message")
        else {
            if first_structured_error.is_none() {
                first_structured_error = Some(AifixError::parser(
                    "cargo compiler-message missing message object",
                ));
            }
            continue;
        };
        let Some(diagnostic) = compiler_message_to_diagnostic(message, value.clone())
        else {
            if first_structured_error.is_none() {
                first_structured_error = Some(AifixError::parser(
                    "cargo compiler-message contained no non-empty message text",
                ));
            }
            continue;
        };
        diagnostics.push(diagnostic);
    }

    if !diagnostics.is_empty() {
        return Ok(diagnostics);
    }
    if let Some(error) = first_structured_error {
        return Err(error);
    }
    if saw_json && !saw_compiler_message {
        return Ok(diagnostics);
    }

    Err(AifixError::parser(
        "clippy JSON input did not contain compiler messages",
    ))
}

/// Convert one rustc compiler message object into a normalized diagnostic.
///
/// # Contract
/// - Preconditions: `message` is the `message` object from a cargo
///   `compiler-message` event and `raw` is the original event.
/// - Postconditions: returns a diagnostic with a non-empty message and
///   preserved raw JSON when a message is available.
/// - Failure modes: returns `None` when the message text is absent or blank.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `message` is the already-normalized non-empty diagnostic
///   text.
/// - Postconditions: returns either `clippy` for clippy-tagged diagnostics or
///   `rustc` otherwise.
/// - Failure modes: none.
/// - Panics: none.
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
/// - Preconditions: `spans` is the optional rustc `spans` JSON array.
/// - Postconditions: returns only primary spans that can be normalized without
///   direct indexing.
/// - Failure modes: malformed or missing spans are skipped.
/// - Panics: none.
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
/// - Preconditions: `spans` is the optional rustc `spans` JSON array.
/// - Postconditions: returns suggestions for spans with
///   `suggested_replacement`, preserving span data when available.
/// - Failure modes: spans without replacement text are skipped.
/// - Panics: none.
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
/// - Preconditions: `span` is a rustc span object with a `file_name` string.
/// - Postconditions: returns a span with one-based rustc coordinates copied
///   when present.
/// - Failure modes: returns `None` when `file_name` is missing or not a string.
/// - Panics: none.
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
/// - Preconditions: `input` is UTF-8 TypeScript compiler text output.
/// - Postconditions: returns diagnostics parsed from `path(line,column):
///   severity TScode: message` lines.
/// - Failure modes: returns a parser error when non-empty input contains no
///   TypeScript diagnostics.
/// - Panics: none.
fn parse_typescript_text(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    let diagnostics = input
        .lines()
        .filter_map(parse_typescript_line)
        .collect::<Vec<_>>();

    if diagnostics.is_empty() && !input.trim().is_empty() {
        return Err(AifixError::parser(
            "typescript text input did not contain TS diagnostics",
        ));
    }

    debug_assert!(
        input.trim().is_empty() || !diagnostics.is_empty(),
        "non-empty successful TypeScript parsing must produce diagnostics"
    );
    Ok(diagnostics)
}

/// Parse one `path(line,column): error TS1234: message` line.
///
/// # Contract
/// - Preconditions: `line` is one logical TypeScript diagnostic line.
/// - Postconditions: returns a normalized `tsc` diagnostic with one span when
///   the line matches.
/// - Failure modes: returns `None` for malformed positions, missing severity,
///   or missing `TS` code/message.
/// - Panics: none; all string slicing is checked.
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
///
/// # Contract
/// - Preconditions: `rest` begins after the TypeScript location prefix.
/// - Postconditions: returns normalized severity and the remainder after the
///   severity word.
/// - Failure modes: returns `None` for unsupported severities.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `rest` begins after the TypeScript severity prefix.
/// - Postconditions: returns an owned `TS...` code and non-empty message.
/// - Failure modes: returns `None` when the delimiter, TS code, or message is
///   missing.
/// - Panics: none; all string slicing is checked.
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

/// Parse LSP diagnostic arrays, wrapper objects, or publishDiagnostics params.
///
/// # Contract
/// - Preconditions: `input` is JSON containing an array, `diagnostics`, or
///   `params.diagnostics`.
/// - Postconditions: returns normalized diagnostics and uses wrapper URI as a
///   fallback span file.
/// - Failure modes: returns JSON errors for malformed JSON or parser errors for
///   missing/non-array diagnostics.
/// - Panics: none.
fn parse_lsp_json(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_lsp_value(&parse_json_value(input)?)
}

/// Convert parsed LSP JSON into normalized diagnostics.
///
/// # Contract
/// - Preconditions: `value` contains an LSP diagnostic array, wrapper object,
///   or publishDiagnostics params object.
/// - Postconditions: returns normalized diagnostics and uses wrapper URI as a
///   fallback span file.
/// - Failure modes: returns parser errors for missing/non-array diagnostics,
///   blank messages, malformed ranges, or reversed ranges.
/// - Panics: none.
fn parse_lsp_value(value: &Value) -> Result<Vec<Diagnostic>, AifixError>
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
/// - Preconditions: `value` is parsed JSON from an LSP adapter boundary.
/// - Postconditions: returns the array candidate from an array, `diagnostics`,
///   or `params.diagnostics` shape without cloning it.
/// - Failure modes: returns a parser error when no LSP diagnostics field is
///   present.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `value` is parsed JSON from an LSP adapter boundary.
/// - Postconditions: returns top-level `uri`, then `params.uri`, preserving the
///   original URI text.
/// - Failure modes: returns `None` when no URI string is present.
/// - Panics: none.
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
/// - Preconditions: `value` is an LSP diagnostic object and `fallback_uri` is
///   the enclosing URI when present.
/// - Postconditions: returns a diagnostic with preserved raw JSON and one
///   validated normalized span.
/// - Failure modes: returns a parser error when message text is absent, blank,
///   or when range validation fails.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `value` is the numeric severity from an LSP diagnostic.
/// - Postconditions: maps 1 through 4 to LSP severities and unknown values to
///   info.
/// - Failure modes: none.
/// - Panics: none.
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
/// - Preconditions: `range` follows the LSP shape with a `start` object;
///   coordinates are zero-based.
/// - Postconditions: returns a span with validated one-based coordinates and an
///   empty file only when no URI is available.
/// - Failure modes: returns a parser error when range objects, coordinates,
///   one-based conversion, or ordering are invalid.
/// - Panics: none.
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
/// - Preconditions: `input` is UTF-8 diagnostic text with one diagnostic per
///   non-empty line.
/// - Postconditions: returns one normalized Nushell diagnostic per non-empty
///   trimmed line.
/// - Failure modes: returns a parser error only when non-empty input yields no
///   diagnostics.
/// - Panics: none.
fn parse_nushell_text(input: &str) -> Result<Vec<Diagnostic>, AifixError>
{
    let diagnostics = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| Diagnostic::new("nushell", None, line_severity(line), line.to_owned()))
        .collect::<Vec<_>>();

    if diagnostics.is_empty() && !input.trim().is_empty() {
        return Err(AifixError::parser(
            "nushell text input did not contain diagnostics",
        ));
    }

    debug_assert!(
        input.trim().is_empty() || !diagnostics.is_empty(),
        "non-empty successful Nushell parsing must produce diagnostics"
    );
    Ok(diagnostics)
}

/// Infer severity from a generic diagnostic line.
///
/// # Contract
/// - Preconditions: `line` is a non-empty trimmed diagnostic line.
/// - Postconditions: returns error for lines containing `error`, warning for
///   `warn`, and info otherwise.
/// - Failure modes: none.
/// - Panics: none.
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
/// - Preconditions: `line` is one logical input line.
/// - Postconditions: returns true only for non-empty lines that look like JSON
///   objects and mention cargo's `reason` or `compiler-message` fields.
/// - Failure modes: none; malformed JSON can still return true so the cargo
///   parser reports the real structured error.
/// - Panics: none.
fn is_cargo_json_line(line: &str) -> bool
{
    let trimmed = line.trim_start();
    trimmed.starts_with('{')
        && (trimmed.contains("\"reason\"") || trimmed.contains("\"compiler-message\""))
}

/// Return whether input starts with a complete-JSON delimiter.
///
/// # Contract
/// - Preconditions: `input` is non-empty UTF-8 text.
/// - Postconditions: returns true for object or array starts after leading
///   whitespace.
/// - Failure modes: none; malformed JSON-looking text still returns true.
/// - Panics: none.
fn looks_like_complete_json(input: &str) -> bool
{
    matches!(input.trim_start().as_bytes().first(), Some(b'{' | b'['))
}

/// Return whether parsed JSON has an LSP diagnostics shape.
///
/// # Contract
/// - Preconditions: `value` is parsed JSON.
/// - Postconditions: returns true for diagnostic arrays, URI-qualified wrapper
///   diagnostics, or publishDiagnostics params with LSP markers.
/// - Failure modes: none; malformed matching shapes are left for LSP parsing.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `value` is parsed JSON.
/// - Postconditions: returns true for arrays that contain a diagnostic with an
///   LSP range marker.
/// - Failure modes: none.
/// - Panics: none.
fn lsp_diagnostics_shape(value: &Value) -> bool
{
    value
        .as_array()
        .is_some_and(|items| items.iter().any(lsp_diagnostic_shape))
}

/// Return whether a value looks like one LSP diagnostic object.
///
/// # Contract
/// - Preconditions: `value` is parsed JSON.
/// - Postconditions: returns true when an LSP diagnostic range is present.
/// - Failure modes: none.
/// - Panics: none.
fn lsp_diagnostic_shape(value: &Value) -> bool
{
    value.get("range").is_some()
}

/// Parse a complete JSON value from input.
///
/// # Contract
/// - Preconditions: `input` is intended to contain one complete JSON value.
/// - Postconditions: returns the parsed [`Value`] without modifying source
///   text.
/// - Failure modes: returns [`AifixError::Json`] for malformed or incomplete
///   JSON.
/// - Panics: none.
fn parse_json_value(input: &str) -> Result<Value, AifixError>
{
    serde_json::from_str(input).map_err(AifixError::Json)
}

/// Return the first string property found on a JSON object.
///
/// # Contract
/// - Preconditions: `keys` is ordered by caller preference and may be empty.
/// - Postconditions: returns an owned copy of the first matching string value.
/// - Failure modes: returns `None` when no key exists or matching values are
///   not strings.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `keys` is ordered by caller preference and may be empty.
/// - Postconditions: trims surrounding whitespace and returns an owned copy of
///   the first matching non-empty string value.
/// - Failure modes: returns `None` when no key exists, matching values are not
///   strings, or all matching strings are blank.
/// - Panics: none.
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
///
/// # Contract
/// - Preconditions: `value` is external text that may include surrounding
///   whitespace.
/// - Postconditions: returns owned trimmed text only when at least one
///   non-whitespace scalar is present.
/// - Failure modes: returns `None` for blank text.
/// - Panics: none.
fn non_empty_trimmed_owned(value: &str) -> Option<String>
{
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Convert a JSON number to `u32` without panicking.
///
/// # Contract
/// - Preconditions: `value` may point to any JSON value.
/// - Postconditions: returns the value only when it is an unsigned integer that
///   fits in `u32`.
/// - Failure modes: returns `None` for missing, non-numeric, negative,
///   fractional, or overflowing values.
/// - Panics: none.
fn json_u32(value: Option<&Value>) -> Option<u32>
{
    let number = value?.as_u64()?;
    u32::try_from(number).ok()
}

/// Parse a decimal `u32` from text.
///
/// # Contract
/// - Preconditions: `value` is a textual coordinate candidate.
/// - Postconditions: trims surrounding whitespace before parsing.
/// - Failure modes: returns `None` for non-decimal, negative, fractional, or
///   overflowing values.
/// - Panics: none.
fn parse_u32_str(value: &str) -> Option<u32>
{
    value.trim().parse::<u32>().ok()
}

/// Convert a zero-based coordinate into a one-based coordinate.
///
/// # Contract
/// - Preconditions: `value` is a zero-based coordinate from an external
///   protocol.
/// - Postconditions: returns `value + 1` when representable.
/// - Failure modes: returns `None` on `u32::MAX` overflow instead of panicking.
/// - Panics: none.
fn one_based(value: u32) -> Option<u32>
{
    value.checked_add(1)
}

/// Unit tests for adapter shape detection and runtime field validation.
#[cfg(test)]
mod tests
{
    use super::parse_diagnostics;
    use crate::error::AifixError;
    use crate::model::Protocol;

    /// Require a condition in adapter unit tests without panicking directly.
    ///
    /// # Contract
    /// - Preconditions: `message` describes the failed invariant.
    /// - Postconditions: returns `Ok(())` when `condition` is true.
    /// - Failure modes: returns [`AifixError::Parser`] when `condition` is
    ///   false.
    /// - Panics: none.
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
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms LSP-shaped blank messages are parser errors
    ///   under auto detection.
    /// - Failure modes: returns a parser error when auto mode falls through to
    ///   generic diagnostics or returns the wrong error category.
    /// - Panics: none.
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

    /// Cargo auto parsing keeps valid compiler diagnostics beside noisy lines.
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms one valid compiler-message survives adjacent
    ///   cargo events, plain noise, and truncated JSON.
    /// - Failure modes: returns a parser error if auto mode drops the valid
    ///   diagnostic or classifies the source incorrectly.
    /// - Panics: none.
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
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms cargo-looking malformed JSON does not fall
    ///   through to generic text when no valid compiler-message exists.
    /// - Failure modes: returns a parser error if auto mode accepts the input
    ///   or reports the wrong error category.
    /// - Panics: none.
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
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms blank source text is normalized before
    ///   [`Diagnostic::new`] is called.
    /// - Failure modes: returns a parser error when the diagnostic is missing
    ///   or the source fallback is not applied.
    /// - Panics: none.
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
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms reversed ranges return a parser error instead
    ///   of reaching [`Span::new`].
    /// - Failure modes: returns a parser error when reversed ranges are
    ///   accepted or classified incorrectly.
    /// - Panics: none.
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

    /// TypeScript parsing rejects empty external fields before model creation.
    ///
    /// # Contract
    /// - Preconditions: parser helpers are available in the test build.
    /// - Postconditions: confirms blank file, code, and message positions fail
    ///   parsing rather than constructing invalid diagnostics.
    /// - Failure modes: returns a parser error when any malformed TypeScript
    ///   line is accepted.
    /// - Panics: none.
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
