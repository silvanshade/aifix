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
use crate::batch::AUTO_PROFILE;
use crate::batch::BatchLimits;
use crate::batch::DEFAULT_BATCH_STREAM_OUTPUT_LIMIT;
use crate::batch::available_profile_names;
use crate::batch::default_protocol_for_profile;
use crate::batch::is_known_profile;
use crate::batch::profile_catalog;
use crate::batch::render_profile_catalog;
use crate::batch::run_auto_profile;
use crate::batch::run_configured_profile;
use crate::batch::run_profile_with_limits;
use crate::batch::unknown_profile_message;
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

/// Server-level guidance returned to MCP clients during initialization.
const SERVER_INSTRUCTIONS: &str = concat!(
    "aifix normalizes diagnostics from parseable tool output; it does not invent fixes or apply changes ",
    "except explicit cached replay apply mode. Use aifix_pipeline for already-captured diagnostics, ",
    "aifix_batch_profiles to discover project profiles before choosing a profile, and aifix_batch with ",
    "profile auto for project-wide diagnostics. Do not invent profile names or assume extraArgs are cargo flags; ",
    "extraArgs are profile-specific and auto rejects them. Treat parseable diagnostics from nonzero exits as ",
    "findings rather than automatic operational failure. Use aifix_dedupe/aifix_guidance for repeated diagnostic ",
    "triage. Use aifix_report_fix/aifix_replay_fixes only for explicitly recorded cached patch replay; respect ",
    "suggest, dry-run, and apply modes. Verify repaired code with the native tools and tests that own it."
);

/// Run the line-oriented stdio MCP server until stdin reaches EOF.
///
/// # Contract
/// - requires: stdin carries UTF-8 newline-delimited JSON-RPC objects and
///   stdout is writable.
/// - ensures: every request receives exactly one JSON response line;
///   notifications receive no response; no logs are written to stdout.
/// - fails: stdin, stdout, or JSON serialization failures are returned as
///   [`AifixError`] values. Tool-level runtime failures are returned inside
///   successful JSON-RPC tool result envelopes.
/// - panics: none.
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
/// - requires: `line` is one line without its trailing newline.
/// - ensures: returns a response object for requests and malformed protocol
///   frames, or `None` for notifications.
/// - fails: malformed JSON is converted to a JSON-RPC parse error.
/// - panics: none.
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
/// - requires: `message` was decoded from a single input line.
/// - ensures: supported request methods return success responses; supported
///   notifications return no response.
/// - fails: invalid requests and unknown protocol methods become JSON-RPC
///   errors.
/// - panics: none.
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
#[must_use]
fn initialize_result() -> Value
{
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "instructions": SERVER_INSTRUCTIONS,
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
                "Run a named diagnostic profile, or omit profile/use auto for project-wide diagnostics; query aifix_batch_profiles when unsure and do not treat extraArgs as cargo flags.",
                &batch_schema(),
            ),
            tool_schema(
                "aifix_batch_profiles",
                "List discoverable batch profiles and metadata for a cwd so agents can choose a valid profile instead of inventing one.",
                &batch_profiles_schema(),
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
#[must_use]
fn batch_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "profile": { "type": "string", "default": AUTO_PROFILE, "description": "Profile name. Omit or pass an empty string to use auto." },
            "extraArgs": { "type": "array", "items": { "type": "string" }, "description": "Profile-specific argv appended to named profiles; rejected for auto and not treated as cargo flags." },
            "cwd": { "type": "string" },
            "protocol": protocol_schema(),
            "format": format_schema(),
            "maxDiagnostics": { "type": "integer", "minimum": 0 },
            "maxOutputBytes": { "type": "integer", "minimum": 0, "description": "Maximum bytes accepted from each invoked-tool output stream." },
            "dedupe": { "type": "boolean" },
            "recordMetrics": { "type": "boolean" },
        },
        "additionalProperties": false,
    })
}

/// Build the batch profile discovery tool schema.
#[must_use]
fn batch_profiles_schema() -> Value
{
    json!({
        "type": "object",
        "properties": {
            "cwd": { "type": "string" },
            "format": format_schema(),
        },
        "additionalProperties": false,
    })
}

/// Build the report-fix tool schema.
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
#[must_use]
fn protocol_schema() -> Value
{
    json!({
        "type": "string",
        "enum": ["auto", "aifix-json", "clippy-json", "typescript-text", "agda-text", "lsp-json", "nushell-text"],
    })
}

/// Build the render format enum schema.
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
/// - requires: `params` is the `tools/call` params value.
/// - ensures: runtime failures are represented as tool errors, not JSON-RPC
///   errors.
/// - panics: none.
#[must_use]
fn tools_call_result(params: Option<&Value>) -> Value
{
    match dispatch_tool_call(params).and_then(tool_result) {
        | Ok(result) => result,
        | Err(error) => tool_error(&error.to_string()),
    }
}

/// Dispatch one decoded tool call and return its result value.
///
/// # Contract
/// - requires: `params` is an arbitrary JSON value supplied by the client.
/// - ensures: known tools are invoked with deserialized arguments.
/// - fails: returns typed argument or runtime errors for `tool-result`
///   encoding.
/// - panics: none.
fn dispatch_tool_call(params: Option<&Value>) -> Result<ToolOutput, AifixError>
{
    let raw_params = params.cloned().unwrap_or(Value::Null);
    let call: ToolCall = serde_json::from_value(raw_params)?;
    match call.name.as_str() {
        | "aifix_pipeline" => run_pipeline_tool(call.arguments),
        | "aifix_batch" => run_batch_tool(call.arguments),
        | "aifix_batch_profiles" => run_batch_profiles_tool(call.arguments),
        | "aifix_report_fix" => run_report_fix_tool(call.arguments),
        | "aifix_replay_fixes" => run_replay_fixes_tool(call.arguments),
        | "aifix_dedupe" => run_dedupe_tool(call.arguments),
        | "aifix_guidance" => run_guidance_tool(call.arguments),
        | name => Err(AifixError::invalid_argument(format!(
            "unknown MCP tool `{name}`"
        ))),
    }
}

/// Convert a successful tool output to an MCP tool result.
///
/// # Contract
/// - requires: `output` was produced by a tool implementation.
/// - ensures: digest output uses existing renderers; JSON output is serialized
///   deterministically by `serde_json`; structured tool errors remain protocol
///   successes with `isError: true`.
/// - fails: JSON serialization failures are returned.
/// - panics: none.
fn tool_result(output: ToolOutput) -> Result<Value, AifixError>
{
    match output {
        | ToolOutput::Text(text) => Ok(tool_success(&text)),
        | ToolOutput::Digest { digest, format } => {
            Ok(tool_success(&render_digest(&digest, format)?))
        },
        | ToolOutput::Json(value) => {
            let text = serde_json::to_string_pretty(&value).map_err(AifixError::from)?;
            Ok(tool_success(&text))
        },
        | ToolOutput::StructuredError {
            text,
            structured_content,
        } => Ok(tool_structured_error(&text, &structured_content)),
    }
}

/// Build a successful MCP tool result.
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

/// Build an MCP tool error result with machine-readable structured content.
#[must_use]
fn tool_structured_error(
    text: &str,
    structured_content: &Value,
) -> Value
{
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "structuredContent": structured_content,
        "isError": true,
    })
}

/// Run the pipeline MCP tool.
///
/// # Contract
/// - requires: arguments contain `input` or `inputPath` and optional
///   parsing/rendering controls.
/// - ensures: parses diagnostics, applies requested cache side effects, and
///   returns a digest rendered by existing renderers.
/// - fails: returns argument, IO, parser, cache, or render setup errors.
/// - panics: none.
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
/// - requires: arguments may omit profile to request auto; optional runtime
///   overrides must match the selected profile's contract.
/// - ensures: mirrors CLI batch config resolution, defaults omitted or empty
///   profile to auto, and applies requested cache side effects before
///   rendering.
/// - fails: returns configuration, process, parser, cache, rendering setup, or
///   structured profile-selection errors.
/// - panics: none.
fn run_batch_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: BatchArgs = serde_json::from_value(arguments)?;
    let cwd = resolve_project_root(args.cwd.as_deref())?;
    let loaded_config = Config::discover(&cwd)?;
    let profile = args
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|profile| !profile.is_empty())
        .unwrap_or(AUTO_PROFILE);
    if !is_known_profile(profile, &loaded_config.config) {
        return Ok(unknown_profile_output(profile, &loaded_config.config));
    }
    if profile == AUTO_PROFILE && !args.extra_args.is_empty() {
        return Ok(auto_extra_args_output(&loaded_config.config));
    }

    let profile_config = loaded_config.config.profiles.get(profile);
    let format = parse_optional_format(args.format.as_deref())?
        .or_else(|| profile_config?.format)
        .or(loaded_config.config.default_format)
        .unwrap_or(OutputFormat::Markdown);
    let profile_limit = profile_config.and_then(|config| config.max_diagnostics);
    let max_diagnostics = args
        .max_diagnostics
        .or(profile_limit)
        .or(loaded_config.config.max_diagnostics);
    let profile_output_limit = profile_config.and_then(|config| config.max_output_bytes);
    let max_output_override = args.max_output_bytes.or(profile_output_limit);
    let mut digest = if profile == AUTO_PROFILE {
        run_auto_profile(
            &loaded_config.config,
            &cwd,
            max_diagnostics,
            max_output_override,
        )
    }
    else {
        let max_output_bytes = max_output_override
            .or(loaded_config.config.max_output_bytes)
            .unwrap_or(DEFAULT_BATCH_STREAM_OUTPUT_LIMIT);
        let limits = BatchLimits::new(max_diagnostics, max_output_bytes);
        let protocol = parse_optional_protocol(args.protocol.as_deref())?
            .or_else(|| profile_config?.protocol)
            .or_else(|| default_protocol_for_profile(profile))
            .or(loaded_config.config.default_protocol)
            .unwrap_or(Protocol::Auto);
        if let Some(profile_config) = profile_config {
            run_configured_profile(
                profile,
                profile_config,
                &args.extra_args,
                protocol,
                &cwd,
                limits,
            )?
        }
        else {
            run_profile_with_limits(profile, &args.extra_args, protocol, &cwd, limits)?
        }
    };
    if args.dedupe || args.record_metrics {
        let diagnostics = apply_optional_cache_updates(
            &cwd,
            digest.diagnostics,
            args.dedupe,
            args.record_metrics,
        )?;
        let invocation = digest.invocation;
        let profile_statuses = digest.profile_statuses;
        digest = build_digest(diagnostics, invocation, max_diagnostics);
        digest.profile_statuses = profile_statuses;
    }

    Ok(ToolOutput::Digest { digest, format })
}

/// Run the batch profile discovery MCP tool.
///
/// # Contract
/// - requires: arguments contain optional `cwd` and `format` fields.
/// - ensures: discovers configuration from `cwd`, renders the shared profile
///   catalog metadata with the same renderer used by the CLI, and performs no
///   tool execution.
/// - fails: returns argument, configuration, IO, or rendering setup errors.
/// - panics: none.
fn run_batch_profiles_tool(arguments: Value) -> Result<ToolOutput, AifixError>
{
    let args: BatchProfilesArgs = serde_json::from_value(arguments)?;
    let cwd = resolve_project_root(args.cwd.as_deref())?;
    let loaded_config = Config::discover(&cwd)?;
    let format = parse_optional_format(args.format.as_deref())?.unwrap_or(OutputFormat::Markdown);
    let profiles = profile_catalog(&loaded_config.config, &cwd);
    let text = render_profile_catalog(&profiles, format)?;

    Ok(ToolOutput::Text(text))
}

/// Build a structured unknown-profile MCP tool error.
#[must_use]
fn unknown_profile_output(
    profile: &str,
    config: &Config,
) -> ToolOutput
{
    let text = unknown_profile_message(profile, config);
    let available_profiles = available_profile_names(config);
    ToolOutput::StructuredError {
        text,
        structured_content: json!({
            "kind": "unknown-profile",
            "profile": profile,
            "available_profiles": available_profiles,
            "recovery_hint": "Call aifix_batch_profiles for this cwd or run `aifix config profiles --format json` before choosing a profile.",
        }),
    }
}

/// Build a structured auto-extra-args MCP tool error.
#[must_use]
fn auto_extra_args_output(config: &Config) -> ToolOutput
{
    let available_profiles = available_profile_names(config);
    ToolOutput::StructuredError {
        text: "batch profile `auto` rejects extraArgs because argument semantics are profile-specific; choose a named profile from aifix_batch_profiles before passing extra arguments.".to_owned(),
        structured_content: json!({
            "kind": "auto-extra-args",
            "profile": AUTO_PROFILE,
            "available_profiles": available_profiles,
            "recovery_hint": "Call aifix_batch_profiles, choose a concrete profile such as rust/typescript/agda/nushell/custom, then pass profile-specific extraArgs only to that profile.",
        }),
    }
}

/// Run the report-fix MCP tool.
///
/// # Contract
/// - requires: arguments include a patch and either a diagnostic or signature.
/// - ensures: records the patch in the project-local cache and returns the
///   canonical signature plus cache path.
/// - fails: returns argument, cache, or IO errors.
/// - panics: none.
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
/// - requires: arguments identify diagnostics directly, through a digest, or
///   through parseable input.
/// - ensures: finds cached fixes, emits per-diagnostic audit JSON, and
///   optionally checks or applies trusted exact patches in the cache layer.
/// - fails: returns argument, parser, cache, or process errors.
/// - panics: none.
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
/// - requires: arguments identify diagnostics directly, through a digest, or
///   through parseable input.
/// - ensures: filters seen diagnostics, records newly surfaced signatures, and
///   renders the surviving digest.
/// - fails: returns argument, parser, cache, IO, or render setup errors.
/// - panics: none.
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
/// - requires: arguments may identify diagnostics for metric recording.
/// - ensures: returns deterministic Markdown guidance from the cache.
/// - fails: returns argument, parser, cache, or IO errors.
/// - panics: none.
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
/// - requires: `diagnostics` are normalized and `project_root` is a UTF-8
///   project root.
/// - ensures: requested metric and dedupe updates are saved before returning;
///   when dedupe is false, diagnostics are returned unchanged.
/// - fails: returns cache loading or saving errors.
/// - panics: none.
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
/// - requires: `path` is supplied by the client or omitted to use the process
///   current directory.
/// - ensures: returns a `Utf8PathBuf` accepted by configuration and cache APIs.
/// - fails: returns IO, UTF-8, or invalid path errors.
/// - panics: none.
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
/// - requires: process current directory is available.
/// - ensures: returns the current directory as a camino UTF-8 path.
/// - fails: returns IO or UTF-8 errors.
/// - panics: none.
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
/// - requires: exactly one of `input` or `input_path` should be supplied.
/// - ensures: returns the UTF-8 input payload.
/// - fails: returns argument or filesystem errors.
/// - panics: none.
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
#[must_use]
fn input_label(input_path: Option<&str>) -> String
{
    input_path.map_or_else(|| "mcp-input".to_owned(), ToOwned::to_owned)
}

/// Parse diagnostics from any accepted diagnostic-bearing argument shape.
///
/// # Contract
/// - requires: caller supplies diagnostics, digest, or input.
/// - ensures: returns normalized diagnostics without mutating the source JSON
///   value.
/// - fails: returns argument, JSON, or parser errors.
/// - panics: none.
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
fn parse_optional_protocol(protocol: Option<&str>) -> Result<Option<Protocol>, AifixError>
{
    protocol
        .map(Protocol::from_str)
        .transpose()
        .map_err(|error| AifixError::invalid_argument(error.to_string()))
}

/// Parse an optional output format string.
fn parse_optional_format(format: Option<&str>) -> Result<Option<OutputFormat>, AifixError>
{
    format
        .map(OutputFormat::from_str)
        .transpose()
        .map_err(|error| AifixError::invalid_argument(error.to_string()))
}

/// Parse an optional replay mode string.
fn parse_replay_mode(mode: Option<&str>) -> Result<ReplayMode, AifixError>
{
    ReplayMode::from_str(mode.unwrap_or("suggest"))
}

/// Top-level MCP tool/call parameters.
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
    /// Tool-level error with machine-readable recovery details.
    StructuredError
    {
        /// Human-readable error text.
        text: String,
        /// Machine-readable structuredContent payload.
        structured_content: Value,
    },
}

/// Arguments accepted by `aifix_pipeline`.
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
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchArgs
{
    /// Profile name to execute; omitted or empty defaults to auto.
    profile: Option<String>,
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
    /// Optional per-stream output byte budget.
    max_output_bytes: Option<usize>,
    /// Whether to filter already-seen diagnostics.
    #[serde(default)]
    dedupe: bool,
    /// Whether to record diagnostic shape metrics.
    #[serde(default)]
    record_metrics: bool,
}

/// Arguments accepted by `aifix_batch_profiles`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchProfilesArgs
{
    /// Working directory for config discovery and project-shape detection.
    cwd: Option<String>,
    /// Optional profile catalog renderer override.
    format: Option<String>,
}

/// Arguments accepted by `aifix_report_fix`.
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

/// Unit coverage for MCP argument schemas and dispatch.
#[cfg(test)]
mod tests
{
    use std::fs;

    use super::*;

    /// Verify camel-case MCP arguments reach the batch output budget.
    #[test]
    fn batch_tool_honors_max_output_bytes() -> Result<(), AifixError>
    {
        let schema = batch_schema();
        assert_eq!(
            schema
                .pointer("/properties/maxOutputBytes/minimum")
                .and_then(Value::as_u64),
            Some(0),
            "batch schema should publish maxOutputBytes"
        );
        let cwd = Utf8PathBuf::from_path_buf(
            std::env::temp_dir().join(format!("aifix-mcp-max-output-bytes-{}", std::process::id())),
        )
        .map_err(|path| {
            AifixError::utf8(format!(
                "temporary MCP test path was not UTF-8: {}",
                path.display()
            ))
        })?;
        drop(fs::remove_dir_all(&cwd));
        fs::create_dir_all(&cwd).map_err(AifixError::io)?;
        fs::write(
            cwd.join("aifix.toml"),
            concat!(
                "[profiles.custom]\n",
                "argv = [\"printf\"]\n",
                "protocol = \"nushell-text\"\n",
            ),
        )
        .map_err(AifixError::io)?;
        let result = run_batch_tool(json!({
            "profile": "custom",
            "extraArgs": ["12345"],
            "cwd": cwd.as_str(),
            "maxOutputBytes": 4_u64
        }));
        fs::remove_dir_all(&cwd).map_err(AifixError::io)?;

        let error = match result {
            | Ok(_) => {
                return Err(AifixError::process(
                    "MCP batch unexpectedly ignored maxOutputBytes",
                ));
            },
            | Err(error) => error,
        };
        assert!(
            error.to_string().contains("capture limit of 4 bytes"),
            "MCP batch should apply camel-case output budget: {error}"
        );
        Ok(())
    }
}
