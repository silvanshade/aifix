//! Stdio Model Context Protocol server for `aifix`.
//!
//! The server speaks newline-delimited JSON-RPC 2.0 on standard input and
//! standard output.  Tool failures are encoded as MCP tool results so the
//! transport remains alive for later requests.

extern crate alloc;

use alloc::borrow::ToOwned as _;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::str::FromStr as _;
use std::io;
use std::io::BufRead as _;
use std::io::Write as _;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::adapter::parse_diagnostics;
use crate::batch::run_configured_profile;
use crate::batch::run_profile_with_limit;
use crate::cache::ReplayMode;
use crate::cache::diagnostic_cache_path;
use crate::cache::filter_unseen_and_save;
use crate::cache::load_diagnostic_cache;
use crate::cache::record_fix_and_save;
use crate::cache::record_guidance_and_save;
use crate::cache::render_guidance_markdown;
use crate::cache::replay_cached_fixes_and_save;
use crate::cache::update_diagnostics_and_save;
use crate::config::Config;
use crate::digest::build_digest;
use crate::error::AifixError;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Invocation;
use crate::model::OutputFormat;
use crate::model::Protocol;
use crate::render::render_digest;

/// JSON-RPC protocol version emitted in every response.
const JSON_RPC_VERSION: &str = "2.0";

/// MCP protocol version advertised by the server.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Server implementation name reported during initialization.
const SERVER_NAME: &str = "aifix";

/// Server implementation version reported during initialization.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the line-oriented stdio MCP server until stdin reaches EOF.
///
/// # Contract
/// - Preconditions: stdin carries UTF-8 newline-delimited JSON-RPC objects and
///   stdout is writable.
/// - Postconditions: every request receives exactly one JSON response line;
///   notifications receive no response; no logs are written to stdout.
/// - Failure modes: stdin, stdout, or JSON serialization failures are returned
///   as [`AifixError`] values. Tool-level runtime failures are returned inside
///   successful JSON-RPC tool result envelopes.
/// - Panics: none.
///
/// # Errors
/// Returns an error when process IO fails or when a response cannot be
/// serialized.
#[inline]
pub fn run_stdio_server() -> Result<(), AifixError>
{
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.map_err(AifixError::io)?;
        let Some(response) = handle_line(&line)
        else {
            continue;
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n").map_err(AifixError::io)?;
        stdout.flush().map_err(AifixError::io)?;
    }

    Ok(())
}

/// Handle one JSON-RPC input line.
///
/// # Contract
/// - Preconditions: `line` is one line without its trailing newline.
/// - Postconditions: returns a response object for requests and malformed
///   protocol frames, or `None` for notifications.
/// - Failure modes: malformed JSON is converted to a JSON-RPC parse error.
/// - Panics: none.
#[must_use]
fn handle_line(line: &str) -> Option<Value>
{
    match serde_json::from_str::<Value>(line) {
        | Ok(value) => handle_message(&value),
        | Err(error) => Some(error_response(
            &Value::Null,
            -32700,
            &format!("parse error: {error}"),
        )),
    }
}

/// Dispatch one decoded JSON-RPC message.
///
/// # Contract
/// - Preconditions: `message` was decoded from a single input line.
/// - Postconditions: supported request methods return success responses;
///   supported notifications return no response.
/// - Failure modes: invalid requests and unknown protocol methods become
///   JSON-RPC errors.
/// - Panics: none.
#[must_use]
fn handle_message(message: &Value) -> Option<Value>
{
    let Some(object) = message.as_object()
    else {
        return Some(error_response(
            &Value::Null,
            -32600,
            "invalid request: expected object",
        ));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str)
    else {
        return Some(error_response(
            &id.unwrap_or(Value::Null),
            -32600,
            "invalid request: missing method",
        ));
    };
    let id = id?;
    let params = object.get("params");

    Some(match method {
        | "initialize" => success_response(&id, &initialize_result()),
        | "ping" => success_response(&id, &json!({})),
        | "tools/list" => success_response(&id, &tools_list_result()),
        | "tools/call" => success_response(&id, &tools_call_result(params)),
        | _ => error_response(&id, -32601, &format!("method not found: {method}")),
    })
}

/// Build a JSON-RPC success response.
///
/// # Contract
/// - Preconditions: `id` is the request identifier and `result` is JSON
///   serializable.
/// - Postconditions: returns a JSON-RPC 2.0 success object.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn success_response(
    id: &Value,
    result: &Value,
) -> Value
{
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": id,
        "result": result,
    })
}

/// Build a JSON-RPC error response.
///
/// # Contract
/// - Preconditions: `code` and `message` describe a protocol-level error.
/// - Postconditions: returns a JSON-RPC 2.0 error object.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn error_response(
    id: &Value,
    code: i32,
    message: &str,
) -> Value
{
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

/// Build the MCP initialize result.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: advertises the supported MCP version, server identity, and
///   tools capability.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn initialize_result() -> Value
{
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION,
        },
        "capabilities": {
            "tools": {},
        },
    })
}

/// Build the MCP tools/list result.
///
/// # Contract
/// - Preconditions: tool definitions below stay aligned with `tools/call`
///   dispatch.
/// - Postconditions: returns all six public tool descriptions and input
///   schemas.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn tools_list_result() -> Value
{
    json!({
        "tools": [
            tool_schema(
                "aifix_pipeline",
                "Parse existing diagnostic output, optionally dedupe or record metrics, and render a digest.",
                &pipeline_schema(),
            ),
            tool_schema(
                "aifix_batch",
                "Run a configured diagnostic profile, then render its digest.",
                &batch_schema(),
            ),
            tool_schema(
                "aifix_report_fix",
                "Record a project-local successful patch for a diagnostic signature.",
                &report_fix_schema(),
            ),
            tool_schema(
                "aifix_replay_fixes",
                "Find cached fixes for diagnostics and return deterministic per-diagnostic audit fields; suggest returns exact and approximate candidates, dry-run checks candidates, and apply only applies exact matches.",
                &replay_fixes_schema(),
            ),
            tool_schema(
                "aifix_dedupe",
                "Filter already-seen diagnostics using the project-local cache.",
                &dedupe_schema(),
            ),
            tool_schema(
                "aifix_guidance",
                "Return deterministic Markdown guidance from project-local diagnostic shape metrics.",
                &guidance_schema(),
            ),
        ],
    })
}

/// Build one MCP tool metadata object.
///
/// # Contract
/// - Preconditions: `name` is the dispatch name and `schema` is a JSON Schema
///   object.
/// - Postconditions: returns MCP-compatible tool metadata.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn tool_schema(
    name: &str,
    description: &str,
    schema: &Value,
) -> Value
{
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

/// Build the shared schema base for digest-rendering tools.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns reusable property definitions for protocol,
///   format, diagnostics limit, project root, dedupe, and metrics toggles.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn pipeline_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "input": { "type": "string" },
            "inputPath": { "type": "string" },
            "protocol": protocol_schema(),
            "format": format_schema(),
            "maxDiagnostics": { "type": "integer", "minimum": 0 },
            "projectRoot": { "type": "string" },
            "dedupe": { "type": "boolean" },
            "recordMetrics": { "type": "boolean" },
        },
        "additionalProperties": false,
    })
}

/// Build the batch tool schema.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns the MCP input schema for `aifix_batch`.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn batch_schema() -> Value
{
    json!({
        "type": "object",
        "required": ["profile"],
        "properties": {
            "profile": { "type": "string" },
            "extraArgs": { "type": "array", "items": { "type": "string" } },
            "cwd": { "type": "string" },
            "protocol": protocol_schema(),
            "format": format_schema(),
            "maxDiagnostics": { "type": "integer", "minimum": 0 },
            "dedupe": { "type": "boolean" },
            "recordMetrics": { "type": "boolean" },
        },
        "additionalProperties": false,
    })
}

/// Build the report-fix tool schema.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns the MCP input schema for `aifix_report_fix`.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn report_fix_schema() -> Value
{
    json!({
        "type": "object",
        "required": ["patch"],
        "properties": {
            "projectRoot": { "type": "string" },
            "diagnostic": { "type": "object" },
            "signature": { "type": "string" },
            "patch": { "type": "string" },
            "note": { "type": "string" },
        },
        "additionalProperties": false,
    })
}

/// Build the replay-fixes tool schema.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns the MCP input schema for `aifix_replay_fixes`.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn replay_fixes_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "projectRoot": { "type": "string" },
            "diagnostics": { "type": "array", "items": { "type": "object" } },
            "digest": { "type": "object" },
            "input": { "type": "string" },
            "protocol": protocol_schema(),
            "mode": {
                "type": "string",
                "enum": ["suggest", "dry-run", "apply"],
                "description": "suggest returns exact and approximate candidates without git; dry-run runs git apply --check for candidates; apply checks and applies exact matches only, reporting approximate skips in result.diagnostics and skipped_approximate_applies."
            },
        },
        "additionalProperties": false,
    })
}

/// Build the dedupe tool schema.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns the MCP input schema for `aifix_dedupe`.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn dedupe_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "projectRoot": { "type": "string" },
            "diagnostics": { "type": "array", "items": { "type": "object" } },
            "digest": { "type": "object" },
            "input": { "type": "string" },
            "protocol": protocol_schema(),
            "format": format_schema(),
            "maxDiagnostics": { "type": "integer", "minimum": 0 },
        },
        "additionalProperties": false,
    })
}

/// Build the guidance tool schema.
///
/// # Contract
/// - Preconditions: none.
/// - Postconditions: returns the MCP input schema for `aifix_guidance`.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn guidance_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "projectRoot": { "type": "string" },
            "diagnostics": { "type": "array", "items": { "type": "object" } },
            "digest": { "type": "object" },
            "input": { "type": "string" },
            "protocol": protocol_schema(),
        },
        "additionalProperties": false,
    })
}

/// Build the protocol enum schema.
///
/// # Contract
/// - Preconditions: spellings mirror [`Protocol::as_str`].
/// - Postconditions: returns a reusable JSON Schema enum.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn protocol_schema() -> Value
{
    json!({
        "type": "string",
        "enum": ["auto", "aifix-json", "clippy-json", "typescript-text", "lsp-json", "nushell-text"],
    })
}

/// Build the render format enum schema.
///
/// # Contract
/// - Preconditions: spellings mirror [`OutputFormat::as_str`].
/// - Postconditions: returns a reusable JSON Schema enum.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn format_schema() -> Value
{
    json!({
        "type": "string",
        "enum": ["json", "compact-json", "markdown"],
    })
}

/// Dispatch an MCP tool call.
///
/// # Contract
/// - Preconditions: `params` is the `tools/call` params value.
/// - Postconditions: runtime failures are represented as tool errors, not
///   JSON-RPC errors.
/// - Failure modes: none at the JSON-RPC layer.
/// - Panics: none.
#[must_use]
fn tools_call_result(params: Option<&Value>) -> Value
{
    let result = dispatch_tool_call(params).and_then(tool_text);
    match result {
        | Ok(text) => tool_success(&text),
        | Err(error) => tool_error(&error.to_string()),
    }
}

/// Dispatch one decoded tool call and return its result value.
///
/// # Contract
/// - Preconditions: `params` is an arbitrary JSON value supplied by the client.
/// - Postconditions: known tools are invoked with deserialized arguments.
/// - Failure modes: returns typed argument or runtime errors for `tool-result`
///   encoding.
/// - Panics: none.
fn dispatch_tool_call(params: Option<&Value>) -> Result<ToolOutput, AifixError>
{
    let raw_params = params.cloned().unwrap_or(Value::Null);
    let call: ToolCall = serde_json::from_value(raw_params)?;
    match call.name.as_str() {
        | "aifix_pipeline" => run_pipeline_tool(call.arguments),
        | "aifix_batch" => run_batch_tool(call.arguments),
        | "aifix_report_fix" => run_report_fix_tool(call.arguments),
        | "aifix_replay_fixes" => run_replay_fixes_tool(call.arguments),
        | "aifix_dedupe" => run_dedupe_tool(call.arguments),
        | "aifix_guidance" => run_guidance_tool(call.arguments),
        | name => Err(AifixError::invalid_argument(format!(
            "unknown MCP tool `{name}`"
        ))),
    }
}

/// Convert a successful tool output to display text.
///
/// # Contract
/// - Preconditions: `output` was produced by a tool implementation.
/// - Postconditions: digest output uses existing renderers; JSON output is
///   serialized deterministically by `serde_json`.
/// - Failure modes: JSON serialization failures are returned.
/// - Panics: none.
fn tool_text(output: ToolOutput) -> Result<String, AifixError>
{
    match output {
        | ToolOutput::Text(text) => Ok(text),
        | ToolOutput::Digest { digest, format } => render_digest(&digest, format),
        | ToolOutput::Json(value) => serde_json::to_string_pretty(&value).map_err(AifixError::from),
    }
}

/// Build a successful MCP tool result.
///
/// # Contract
/// - Preconditions: `text` is human-readable tool output.
/// - Postconditions: returns one text content part with `isError` false.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn tool_success(text: &str) -> Value
{
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "isError": false,
    })
}

/// Build an MCP tool error result.
///
/// # Contract
/// - Preconditions: `text` describes a runtime tool failure.
/// - Postconditions: returns one text content part with `isError` true.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn tool_error(text: &str) -> Value
{
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "isError": true,
    })
}

/// Run the pipeline MCP tool.
///
/// # Contract
/// - Preconditions: arguments contain `input` or `inputPath` and optional
///   parsing/rendering controls.
/// - Postconditions: parses diagnostics, applies requested cache side effects,
///   and returns a digest rendered by existing renderers.
/// - Failure modes: returns argument, IO, parser, cache, or render setup
///   errors.
/// - Panics: none.
fn run_pipeline_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: PipelineArgs = serde_json::from_value(arguments)?;
    let project_root = resolve_project_root(args.project_root.as_deref())?;
    let protocol = parse_optional_protocol(args.protocol.as_deref())?.unwrap_or(Protocol::Auto);
    let format = parse_optional_format(args.format.as_deref())?.unwrap_or(OutputFormat::Markdown);
    let input = read_tool_input(args.input.as_deref(), args.input_path.as_deref())?;
    let diagnostics = parse_diagnostics(protocol, &input)?;
    let diagnostics =
        apply_optional_cache_updates(&project_root, diagnostics, args.dedupe, args.record_metrics)?;
    let digest = build_digest(
        diagnostics,
        Invocation::pipeline(protocol, input_label(args.input_path.as_deref())),
        args.max_diagnostics,
    );

    Ok(ToolOutput::Digest { digest, format })
}

/// Run the batch MCP tool.
///
/// # Contract
/// - Preconditions: arguments contain a non-empty profile and optional runtime
///   overrides.
/// - Postconditions: mirrors CLI batch config resolution and applies requested
///   cache side effects before rendering.
/// - Failure modes: returns configuration, process, parser, cache, or rendering
///   setup errors.
/// - Panics: none.
fn run_batch_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: BatchArgs = serde_json::from_value(arguments)?;
    if args.profile.is_empty() {
        return Err(AifixError::invalid_argument(
            "batch profile must not be empty",
        ));
    }
    let cwd = resolve_project_root(args.cwd.as_deref())?;
    let loaded_config = Config::discover(&cwd)?;
    let profile_config = loaded_config.config.profiles.get(&args.profile);
    let protocol = parse_optional_protocol(args.protocol.as_deref())?
        .or_else(|| profile_config?.protocol)
        .or(loaded_config.config.default_protocol)
        .unwrap_or(Protocol::Auto);
    let format = parse_optional_format(args.format.as_deref())?
        .or_else(|| profile_config?.format)
        .or(loaded_config.config.default_format)
        .unwrap_or(OutputFormat::Markdown);
    let profile_limit = profile_config.and_then(|profile| profile.max_diagnostics);
    let max_diagnostics = args
        .max_diagnostics
        .or(profile_limit)
        .or(loaded_config.config.max_diagnostics);
    let digest = if let Some(profile) = profile_config {
        run_configured_profile(
            &args.profile,
            profile,
            &args.extra_args,
            protocol,
            &cwd,
            max_diagnostics,
        )?
    }
    else {
        run_profile_with_limit(
            &args.profile,
            &args.extra_args,
            protocol,
            &cwd,
            max_diagnostics,
        )?
    };
    let diagnostics =
        apply_optional_cache_updates(&cwd, digest.diagnostics, args.dedupe, args.record_metrics)?;
    let digest = build_digest(diagnostics, digest.invocation, max_diagnostics);

    Ok(ToolOutput::Digest { digest, format })
}

/// Run the report-fix MCP tool.
///
/// # Contract
/// - Preconditions: arguments include a patch and either a diagnostic or
///   signature.
/// - Postconditions: records the patch in the project-local cache and returns
///   the canonical signature plus cache path.
/// - Failure modes: returns argument, cache, or IO errors.
/// - Panics: none.
fn run_report_fix_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: ReportFixArgs = serde_json::from_value(arguments)?;
    let project_root = resolve_project_root(args.project_root.as_deref())?;
    let signature = record_fix_and_save(
        Some(&project_root),
        args.diagnostic.as_ref(),
        args.signature.as_deref(),
        args.patch,
        args.note,
    )?;

    Ok(ToolOutput::Json(json!({
        "signature": signature,
        "cachePath": diagnostic_cache_path(&project_root),
    })))
}

/// Run the replay-fixes MCP tool.
///
/// # Contract
/// - Preconditions: arguments identify diagnostics directly, through a digest,
///   or through parseable input.
/// - Postconditions: finds cached fixes, emits per-diagnostic audit JSON, and
///   optionally checks or applies trusted exact patches in the cache layer.
/// - Failure modes: returns argument, parser, cache, or process errors.
/// - Panics: none.
fn run_replay_fixes_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: ReplayFixesArgs = serde_json::from_value(arguments)?;
    let project_root = resolve_project_root(args.project_root.as_deref())?;
    let diagnostics = diagnostics_from_args(
        args.diagnostics.as_ref(),
        args.digest.as_ref(),
        args.input.as_deref(),
        args.protocol.as_deref(),
    )?;
    let mode = parse_replay_mode(args.mode.as_deref())?;
    let result = replay_cached_fixes_and_save(Some(&project_root), &diagnostics, mode)?;
    let value = json!({ "result": result });

    Ok(ToolOutput::Json(value))
}

/// Run the dedupe MCP tool.
///
/// # Contract
/// - Preconditions: arguments identify diagnostics directly, through a digest,
///   or through parseable input.
/// - Postconditions: filters seen diagnostics, records newly surfaced
///   signatures, and renders the surviving digest.
/// - Failure modes: returns argument, parser, cache, IO, or render setup
///   errors.
/// - Panics: none.
fn run_dedupe_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: DedupeArgs = serde_json::from_value(arguments)?;
    let project_root = resolve_project_root(args.project_root.as_deref())?;
    let format = parse_optional_format(args.format.as_deref())?.unwrap_or(OutputFormat::Markdown);
    let protocol = parse_optional_protocol(args.protocol.as_deref())?.unwrap_or(Protocol::Auto);
    let diagnostics = diagnostics_from_args(
        args.diagnostics.as_ref(),
        args.digest.as_ref(),
        args.input.as_deref(),
        args.protocol.as_deref(),
    )?;
    let unseen = filter_unseen_and_save(Some(&project_root), &diagnostics)?;
    let digest = build_digest(
        unseen,
        Invocation::pipeline(protocol, "mcp-dedupe"),
        args.max_diagnostics,
    );

    Ok(ToolOutput::Digest { digest, format })
}

/// Run the guidance MCP tool.
///
/// # Contract
/// - Preconditions: arguments may identify diagnostics for metric recording.
/// - Postconditions: returns deterministic Markdown guidance from the cache.
/// - Failure modes: returns argument, parser, cache, or IO errors.
/// - Panics: none.
fn run_guidance_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: GuidanceArgs = serde_json::from_value(arguments)?;
    let project_root = resolve_project_root(args.project_root.as_deref())?;
    let guidance = if has_diagnostic_input(
        args.diagnostics.as_ref(),
        args.digest.as_ref(),
        args.input.as_deref(),
    ) {
        let diagnostics = diagnostics_from_args(
            args.diagnostics.as_ref(),
            args.digest.as_ref(),
            args.input.as_deref(),
            args.protocol.as_deref(),
        )?;
        record_guidance_and_save(Some(&project_root), &diagnostics)?
    }
    else {
        let cache = load_diagnostic_cache(&project_root)?;
        render_guidance_markdown(&cache)
    };

    Ok(ToolOutput::Text(guidance))
}

/// Apply optional dedupe and metric cache updates to diagnostics.
///
/// # Contract
/// - Preconditions: `diagnostics` are normalized and `project_root` is a UTF-8
///   project root.
/// - Postconditions: requested metric and dedupe updates are saved before
///   returning; when dedupe is false, diagnostics are returned unchanged.
/// - Failure modes: returns cache loading or saving errors.
/// - Panics: none.
fn apply_optional_cache_updates(
    project_root: &Utf8Path,
    diagnostics: Vec<Diagnostic>,
    dedupe: bool,
    record_metrics: bool,
) -> Result<Vec<Diagnostic>, AifixError>
{
    if !dedupe && !record_metrics {
        return Ok(diagnostics);
    }
    update_diagnostics_and_save(Some(project_root), &diagnostics, dedupe, record_metrics)
}

/// Resolve an optional project-root string into a UTF-8 absolute or relative
/// path.
///
/// # Contract
/// - Preconditions: `path` is supplied by the client or omitted to use the
///   process current directory.
/// - Postconditions: returns a `Utf8PathBuf` accepted by configuration and
///   cache APIs.
/// - Failure modes: returns IO, UTF-8, or invalid path errors.
/// - Panics: none.
fn resolve_project_root(path: Option<&str>) -> Result<Utf8PathBuf, AifixError>
{
    match path {
        | Some(value) => {
            if value.is_empty() {
                return Err(AifixError::invalid_argument(
                    "project root must not be empty",
                ));
            }
            Ok(Utf8PathBuf::from(value))
        },
        | None => current_utf8_dir(),
    }
}

/// Return the current process directory as UTF-8.
///
/// # Contract
/// - Preconditions: process current directory is available.
/// - Postconditions: returns the current directory as a camino UTF-8 path.
/// - Failure modes: returns IO or UTF-8 errors.
/// - Panics: none.
fn current_utf8_dir() -> Result<Utf8PathBuf, AifixError>
{
    let cwd = std::env::current_dir().map_err(AifixError::io)?;
    Utf8PathBuf::from_path_buf(cwd).map_err(|path| {
        AifixError::utf8(format!(
            "current directory is not valid UTF-8: {}",
            path.display()
        ))
    })
}

/// Read inline or path-based tool input.
///
/// # Contract
/// - Preconditions: exactly one of `input` or `input_path` should be supplied.
/// - Postconditions: returns the UTF-8 input payload.
/// - Failure modes: returns argument or filesystem errors.
/// - Panics: none.
fn read_tool_input(
    input: Option<&str>,
    input_path: Option<&str>,
) -> Result<String, AifixError>
{
    match (input, input_path) {
        | (Some(_), Some(_)) => Err(AifixError::invalid_argument(
            "provide either `input` or `inputPath`, not both",
        )),
        | (Some(value), None) => Ok(value.to_owned()),
        | (None, Some(path)) => {
            if path.is_empty() {
                return Err(AifixError::invalid_argument("inputPath must not be empty"));
            }
            std::fs::read_to_string(path).map_err(AifixError::io)
        },
        | (None, None) => Err(AifixError::invalid_argument(
            "provide `input` or `inputPath`",
        )),
    }
}

/// Produce a stable synthetic input label for MCP pipeline invocations.
///
/// # Contract
/// - Preconditions: `input_path` is the optional source path argument.
/// - Postconditions: returns the path when present, otherwise an MCP stdin-like
///   label.
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn input_label(input_path: Option<&str>) -> String
{
    input_path.map_or_else(|| "mcp-input".to_owned(), ToOwned::to_owned)
}

/// Parse diagnostics from any accepted diagnostic-bearing argument shape.
///
/// # Contract
/// - Preconditions: caller supplies diagnostics, digest, or input.
/// - Postconditions: returns normalized diagnostics without mutating the source
///   JSON value.
/// - Failure modes: returns argument, JSON, or parser errors.
/// - Panics: none.
fn diagnostics_from_args(
    diagnostics: Option<&Value>,
    digest_value: Option<&Value>,
    input: Option<&str>,
    protocol: Option<&str>,
) -> Result<Vec<Diagnostic>, AifixError>
{
    match (diagnostics, digest_value, input) {
        | (Some(value), None, None) => {
            serde_json::from_value(value.clone()).map_err(AifixError::from)
        },
        | (None, Some(value), None) => {
            let parsed_digest: Digest = serde_json::from_value(value.clone())?;
            Ok(parsed_digest.diagnostics)
        },
        | (None, None, Some(value)) => {
            let parsed_protocol = parse_optional_protocol(protocol)?.unwrap_or(Protocol::Auto);
            parse_diagnostics(parsed_protocol, value)
        },
        | (None, None, None) => Err(AifixError::invalid_argument(
            "provide diagnostics, digest, or input",
        )),
        | _ => Err(AifixError::invalid_argument(
            "provide only one of diagnostics, digest, or input",
        )),
    }
}

/// Test whether guidance arguments contain diagnostics to record.
///
/// # Contract
/// - Preconditions: arguments are raw optional fields from the guidance call.
/// - Postconditions: returns true when exactly one diagnostic-bearing source is
///   present, leaving detailed validation to [`diagnostics_from_args`].
/// - Failure modes: none.
/// - Panics: none.
#[must_use]
fn has_diagnostic_input(
    diagnostics: Option<&Value>,
    digest: Option<&Value>,
    input: Option<&str>,
) -> bool
{
    diagnostics.is_some() || digest.is_some() || input.is_some()
}

/// Parse an optional protocol string.
///
/// # Contract
/// - Preconditions: client spellings use the public kebab-case protocol names.
/// - Postconditions: returns `None` for omitted values and a protocol for valid
///   values.
/// - Failure modes: invalid spellings return an argument error.
/// - Panics: none.
fn parse_optional_protocol(protocol: Option<&str>) -> Result<Option<Protocol>, AifixError>
{
    protocol
        .map(Protocol::from_str)
        .transpose()
        .map_err(|error| AifixError::invalid_argument(error.to_string()))
}

/// Parse an optional output format string.
///
/// # Contract
/// - Preconditions: client spellings use the public kebab-case format names.
/// - Postconditions: returns `None` for omitted values and a format for valid
///   values.
/// - Failure modes: invalid spellings return an argument error.
/// - Panics: none.
fn parse_optional_format(format: Option<&str>) -> Result<Option<OutputFormat>, AifixError>
{
    format
        .map(OutputFormat::from_str)
        .transpose()
        .map_err(|error| AifixError::invalid_argument(error.to_string()))
}

/// Parse an optional replay mode string.
///
/// # Contract
/// - Preconditions: client spellings use `suggest`, `dry-run`, or `apply`.
/// - Postconditions: omitted values default to suggest mode.
/// - Failure modes: invalid spellings return an argument error.
/// - Panics: none.
fn parse_replay_mode(mode: Option<&str>) -> Result<ReplayMode, AifixError>
{
    ReplayMode::from_str(mode.unwrap_or("suggest"))
}

/// Top-level MCP tool/call parameters.
///
/// # Contract
/// - Preconditions: deserialized from an MCP `tools/call` request.
/// - Postconditions: preserves the tool name and raw arguments for later typed
///   deserialization.
/// - Failure modes: serde reports missing or mistyped fields.
/// - Panics: none.
#[derive(Debug, Deserialize)]
struct ToolCall
{
    /// Tool name to invoke.
    name: String,
    /// Tool-specific argument object.
    #[serde(default)]
    arguments: Value,
}

/// Successful tool output before MCP content wrapping.
///
/// # Contract
/// - Preconditions: variants are constructed only after a tool succeeds.
/// - Postconditions: keeps renderer selection explicit for digest outputs.
/// - Failure modes: inert until converted to text.
/// - Panics: none.
enum ToolOutput
{
    /// Pre-rendered text output.
    Text(String),
    /// Digest output that must use the existing renderers.
    Digest
    {
        /// Digest to render.
        digest: Digest,
        /// Requested renderer format.
        format: OutputFormat,
    },
    /// JSON value to serialize as pretty text.
    Json(Value),
}

/// Arguments accepted by `aifix_pipeline`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports mistyped fields.
/// - Panics: none.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PipelineArgs
{
    /// Inline diagnostic input.
    input: Option<String>,
    /// Path to diagnostic input.
    input_path: Option<String>,
    /// Optional protocol override.
    protocol: Option<String>,
    /// Optional renderer override.
    format: Option<String>,
    /// Optional sample cap.
    max_diagnostics: Option<usize>,
    /// Optional project root for cache operations.
    project_root: Option<String>,
    /// Whether to filter already-seen diagnostics.
    #[serde(default)]
    dedupe: bool,
    /// Whether to record diagnostic shape metrics.
    #[serde(default)]
    record_metrics: bool,
}

/// Arguments accepted by `aifix_batch`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports missing or mistyped fields.
/// - Panics: none.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchArgs
{
    /// Profile name to execute.
    profile: String,
    /// Extra argv values appended to the selected profile.
    #[serde(default)]
    extra_args: Vec<String>,
    /// Working directory for config discovery and process execution.
    cwd: Option<String>,
    /// Optional protocol override.
    protocol: Option<String>,
    /// Optional renderer override.
    format: Option<String>,
    /// Optional sample cap.
    max_diagnostics: Option<usize>,
    /// Whether to filter already-seen diagnostics.
    #[serde(default)]
    dedupe: bool,
    /// Whether to record diagnostic shape metrics.
    #[serde(default)]
    record_metrics: bool,
}

/// Arguments accepted by `aifix_report_fix`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports missing or mistyped fields.
/// - Panics: none.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportFixArgs
{
    /// Optional project root for cache resolution.
    project_root: Option<String>,
    /// Diagnostic whose stable signature should own the patch.
    diagnostic: Option<Diagnostic>,
    /// Precomputed diagnostic signature.
    signature: Option<String>,
    /// Unified diff patch text to store.
    patch: String,
    /// Optional human note for the stored fix.
    note: Option<String>,
}

/// Arguments accepted by `aifix_replay_fixes`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports mistyped fields.
/// - Panics: none.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayFixesArgs
{
    /// Optional project root for cache resolution.
    project_root: Option<String>,
    /// Direct diagnostics array.
    diagnostics: Option<Value>,
    /// Direct digest object.
    digest: Option<Value>,
    /// Raw diagnostic input to parse.
    input: Option<String>,
    /// Optional protocol used with raw input.
    protocol: Option<String>,
    /// Replay mode selection.
    mode: Option<String>,
}

/// Arguments accepted by `aifix_dedupe`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports mistyped fields.
/// - Panics: none.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DedupeArgs
{
    /// Optional project root for cache resolution.
    project_root: Option<String>,
    /// Direct diagnostics array.
    diagnostics: Option<Value>,
    /// Direct digest object.
    digest: Option<Value>,
    /// Raw diagnostic input to parse.
    input: Option<String>,
    /// Optional protocol used with raw input.
    protocol: Option<String>,
    /// Optional renderer override.
    format: Option<String>,
    /// Optional sample cap.
    max_diagnostics: Option<usize>,
}

/// Arguments accepted by `aifix_guidance`.
///
/// # Contract
/// - Preconditions: deserialized from MCP JSON.
/// - Postconditions: field names mirror the public MCP contract.
/// - Failure modes: serde reports mistyped fields.
/// - Panics: none.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GuidanceArgs
{
    /// Optional project root for cache resolution.
    project_root: Option<String>,
    /// Direct diagnostics array.
    diagnostics: Option<Value>,
    /// Direct digest object.
    digest: Option<Value>,
    /// Raw diagnostic input to parse.
    input: Option<String>,
    /// Optional protocol used with raw input.
    protocol: Option<String>,
}
