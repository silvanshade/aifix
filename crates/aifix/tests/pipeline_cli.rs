//! End-to-end CLI coverage for diagnostic ingestion paths.

/// CLI integration tests for real binary and fixture-based ingestion scenarios.
#[cfg(test)]
mod tests
{
    use core::error::Error;
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::process::Output;
    use std::process::Stdio;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use serde_json::Value;
    use serde_json::json;

    /// Returns the test-built `aifix` binary path provided by Cargo.
    ///
    /// # Contract
    /// Preconditions: Cargo built the integration-test binary with
    /// `CARGO_BIN_EXE_aifix`. Postconditions: returns the executable path
    /// without allocation. Failure modes: compile-time environment lookup
    /// fails before tests run. Panics: none.
    fn binary_path() -> &'static str
    {
        env!("CARGO_BIN_EXE_aifix")
    }

    /// Runs the binary with the supplied arguments and captures the full
    /// output.
    ///
    /// # Contract
    /// Preconditions: Cargo exposed a runnable `aifix` binary for this test
    /// process. Postconditions: returns captured status, stdout, and stderr
    /// without interpreting them. Failure modes: process-spawn errors are
    /// returned to the test. Panics: none.
    fn run_aifix<I, S>(args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(Command::new(binary_path()).args(args).output()?)
    }

    /// Captured MCP subprocess transcript used by stdio integration tests.
    ///
    /// # Contract
    /// Preconditions: values come from one completed `aifix mcp` subprocess.
    /// Postconditions: stores parsed stdout JSON-RPC responses alongside stderr
    /// text for later assertion failures. Failure modes: none while held as a
    /// value. Panics: none.
    struct McpOutput
    {
        /// Parsed newline-delimited JSON-RPC response objects from stdout.
        responses: Vec<Value>,
        /// Complete stderr text decoded lossily for failure messages.
        stderr: String,
    }

    /// Runs the MCP server as a real stdio JSON-RPC subprocess.
    ///
    /// # Contract
    /// Preconditions: Cargo exposed a runnable `aifix` binary and `requests`
    /// contains JSON-RPC objects. Postconditions: writes one JSON request per
    /// line, closes stdin, waits for process exit, and parses each stdout line
    /// as JSON. Failure modes: process, stdin, non-zero exit, UTF-8, empty
    /// response, or JSON parse errors are returned with stderr preserved.
    /// Panics: none.
    fn run_mcp(requests: &[Value]) -> Result<McpOutput, Box<dyn Error>>
    {
        let mut child = Command::new(binary_path())
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("MCP subprocess stdin should be piped"))?;
        for request in requests {
            serde_json::to_writer(&mut *stdin, request)?;
            stdin.write_all(b"\n")?;
        }
        stdin.flush()?;
        drop(child.stdin.take());

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let status_code = output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string());
            return Err(std::io::Error::other(format!(
                "aifix mcp exited with status {status_code}; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }

        let stdout = String::from_utf8(output.stdout)?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let mut responses = Vec::new();
        for (index, line) in stdout.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let response = serde_json::from_str(line).map_err(|error| {
                std::io::Error::other(format!(
                    "MCP stdout line {index} was not JSON: {error}; line: {line}; stderr: {stderr}"
                ))
            })?;
            responses.push(response);
        }
        require(!responses.is_empty(), || {
            format!("MCP subprocess should emit at least one response; stderr: {stderr}")
        })?;
        Ok(McpOutput { responses, stderr })
    }

    /// Builds the JSON-RPC initialize request shared by MCP integration tests.
    ///
    /// # Contract
    /// Preconditions: `id` is unique within the request batch. Postconditions:
    /// returns a standards-shaped initialize request with deterministic client
    /// metadata. Failure modes: none. Panics: none.
    fn mcp_initialize(id: u64) -> Value
    {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "aifix-integration-tests",
                    "version": "0"
                }
            }
        })
    }

    /// Builds the JSON-RPC initialized notification expected after initialize.
    ///
    /// # Contract
    /// Preconditions: none. Postconditions: returns a notification without an
    /// id so the server must not emit a response for it. Failure modes: none.
    /// Panics: none.
    fn mcp_initialized_notification() -> Value
    {
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        })
    }

    /// Builds an MCP `tools/call` request.
    ///
    /// # Contract
    /// Preconditions: `name` names a requested MCP tool and `arguments` is an
    /// object accepted by that tool. Postconditions: returns one JSON-RPC
    /// request. Failure modes: none. Panics: none.
    fn mcp_tool_call(
        id: u64,
        name: &str,
        arguments: &Value,
    ) -> Value
    {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        })
    }

    /// Finds a JSON-RPC response by numeric id.
    ///
    /// # Contract
    /// Preconditions: `output` came from `run_mcp`. Postconditions: returns the
    /// matching response object. Failure modes: missing response ids are
    /// returned with the full response batch and stderr for diagnosis. Panics:
    /// none.
    fn mcp_response_by_id(
        output: &McpOutput,
        id: u64,
    ) -> Result<&Value, Box<dyn Error>>
    {
        output
            .responses
            .iter()
            .find(|response| response.get("id").and_then(Value::as_u64) == Some(id))
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "MCP response id {id} should be present in responses: {:?}; stderr: {}",
                    output.responses, output.stderr
                ))
                .into()
            })
    }

    /// Returns a successful JSON-RPC result object.
    ///
    /// # Contract
    /// Preconditions: `response` is a JSON-RPC response. Postconditions:
    /// returns the result field after rejecting protocol errors. Failure modes:
    /// JSON-RPC error or missing result are returned to the test. Panics: none.
    fn mcp_result(response: &Value) -> Result<&Value, Box<dyn Error>>
    {
        require(response.get("error").is_none(), || {
            format!("MCP response should not contain a protocol error: {response}")
        })?;
        response.get("result").ok_or_else(|| {
            std::io::Error::other(format!("MCP response should contain a result: {response}"))
                .into()
        })
    }

    /// Returns the concatenated text content from a successful MCP tool result.
    ///
    /// # Contract
    /// Preconditions: `response` is a `tools/call` response. Postconditions:
    /// returns all text content after validating `isError` is not true. Failure
    /// modes: tool errors, missing content, or missing text are returned to the
    /// test. Panics: none.
    fn mcp_tool_text(response: &Value) -> Result<String, Box<dyn Error>>
    {
        let result = mcp_result(response)?;
        require(
            result.get("isError").and_then(Value::as_bool) != Some(true),
            || format!("MCP tool result should not be an error: {response}"),
        )?;
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "MCP tool result should contain content: {response}"
                ))
            })?;
        let mut text = String::new();
        for item in content {
            if item.get("type").and_then(Value::as_str) == Some("text")
                && let Some(segment) = item.get("text").and_then(Value::as_str)
            {
                text.push_str(segment);
                text.push('\n');
            }
        }
        require(!text.is_empty(), || {
            format!("MCP tool result should contain text content: {response}")
        })?;
        Ok(text)
    }

    /// Writes a uniquely named temporary input file for a CLI scenario.
    ///
    /// # Contract
    /// Preconditions: `name` is non-empty and contains only filename-safe test
    /// text. Postconditions: returns the path to a file containing exactly
    /// `contents`. Failure modes: empty fixture names, system-clock errors,
    /// omitted file names, or filesystem errors are returned to the test.
    /// Panics: none.
    fn write_temp_input(
        name: &str,
        contents: &str,
    ) -> Result<PathBuf, Box<dyn Error>>
    {
        require(!name.is_empty(), || {
            "temporary CLI fixture names should identify the scenario".to_owned()
        })?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path =
            std::env::temp_dir().join(format!("aifix-{name}-{}-{stamp}.txt", std::process::id()));
        require(path.file_name().is_some(), || {
            format!(
                "temporary CLI fixture path should include a file name: {}",
                path.display()
            )
        })?;
        fs::write(&path, contents)?;
        Ok(path)
    }

    /// Creates a uniquely named temporary directory for a CLI scenario.
    ///
    /// # Contract
    /// Preconditions: `name` is non-empty and contains only filename-safe test
    /// text. Postconditions: returns the path to an existing directory unique
    /// to this test process and timestamp. Failure modes:
    /// empty fixture names, system-clock errors, omitted directory names, or
    /// filesystem errors are returned to the test. Panics: none.
    fn create_temp_dir(name: &str) -> Result<PathBuf, Box<dyn Error>>
    {
        require(!name.is_empty(), || {
            "temporary CLI directory names should identify the scenario".to_owned()
        })?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path =
            std::env::temp_dir().join(format!("aifix-{name}-{}-{stamp}", std::process::id()));
        require(path.file_name().is_some(), || {
            format!(
                "temporary CLI directory path should include a final component: {}",
                path.display()
            )
        })?;
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Converts successful command output into a UTF-8 string.
    ///
    /// # Contract
    /// Preconditions: `output` came from `run_aifix`.
    /// Postconditions: returns stdout as valid UTF-8 after validating
    /// successful process exit. Failure modes: unsuccessful exit or invalid
    /// UTF-8 is returned to the test. Panics: none.
    fn successful_stdout(output: Output) -> Result<String, Box<dyn Error>>
    {
        let status_code = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        require(
            output.status.code().is_some() || !output.status.success(),
            || {
                "successful CLI output should have an exit code on supported test platforms"
                    .to_owned()
            },
        )?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "aifix exited with status {status_code}; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }

        Ok(String::from_utf8(output.stdout)?)
    }

    /// Converts failing command stderr into a UTF-8 string.
    ///
    /// # Contract
    /// Preconditions: `output` came from `run_aifix`.
    /// Postconditions: returns stderr as valid UTF-8 after validating failed
    /// process exit. Failure modes: successful exit or invalid UTF-8 is
    /// returned to the test. Panics: none.
    fn unsuccessful_stderr(output: Output) -> Result<String, Box<dyn Error>>
    {
        require(!output.status.success(), || {
            format!(
                "aifix should have failed; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })?;

        Ok(String::from_utf8(output.stderr)?)
    }

    /// Parses JSON stdout from a successful command.
    ///
    /// # Contract
    /// Preconditions: `output` came from a command requested to render JSON.
    /// Postconditions: returns parsed JSON after validating successful CLI
    /// exit. Failure modes: unsuccessful exit, empty stdout, invalid UTF-8, or
    /// invalid JSON is returned. Panics: none.
    fn successful_json(output: Output) -> Result<Value, Box<dyn Error>>
    {
        let stdout = successful_stdout(output)?;
        require(!stdout.trim_start().is_empty(), || {
            "JSON CLI output should not be empty".to_owned()
        })?;
        Ok(serde_json::from_str(&stdout)?)
    }

    /// Returns true when any JSON string leaf contains `needle`.
    ///
    /// # Contract
    /// Preconditions: `needle` is the exact substring expected in the rendered
    /// digest. Postconditions: returns true if any reachable string leaf
    /// contains `needle`. Failure modes: none.
    /// Panics: none.
    fn json_contains_str(
        value: &Value,
        needle: &str,
    ) -> bool
    {
        match *value {
            | Value::String(ref text) => text.contains(needle),
            | Value::Array(ref items) => items.iter().any(|item| json_contains_str(item, needle)),
            | Value::Object(ref fields) => {
                fields.values().any(|item| json_contains_str(item, needle))
            },
            | Value::Null | Value::Bool(_) | Value::Number(_) => false,
        }
    }

    /// Returns true when any JSON number leaf equals `expected`.
    ///
    /// # Contract
    /// Preconditions: `expected` is representable as an unsigned JSON number.
    /// Postconditions: returns true if any reachable numeric leaf equals
    /// `expected`. Failure modes: none.
    /// Panics: none.
    fn json_contains_u64(
        value: &Value,
        expected: u64,
    ) -> bool
    {
        match *value {
            | Value::Number(ref number) => number.as_u64() == Some(expected),
            | Value::Array(ref items) => items.iter().any(|item| json_contains_u64(item, expected)),
            | Value::Object(ref fields) => fields
                .values()
                .any(|item| json_contains_u64(item, expected)),
            | Value::Null | Value::Bool(_) | Value::String(_) => false,
        }
    }

    /// Returns true when any object field named `field` equals `expected`.
    ///
    /// # Contract
    /// Preconditions: `field` is the exact object key under inspection.
    /// Postconditions: returns true if any reachable object has the requested
    /// unsigned value. Failure modes: none.
    /// Panics: none.
    fn json_field_equals_u64(
        value: &Value,
        field: &str,
        expected: u64,
    ) -> bool
    {
        match *value {
            | Value::Array(ref items) => items
                .iter()
                .any(|item| json_field_equals_u64(item, field, expected)),
            | Value::Object(ref fields) => {
                fields.get(field).and_then(Value::as_u64) == Some(expected)
                    || fields
                        .values()
                        .any(|item| json_field_equals_u64(item, field, expected))
            },
            | Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
        }
    }

    /// Converts a filesystem path into the UTF-8 CLI path expected by `aifix`.
    ///
    /// # Contract
    /// Preconditions: `path` identifies an existing or planned CLI input.
    /// Postconditions: returns a borrowed string slice without allocation when
    /// the path is UTF-8. Failure modes: non-UTF-8 paths are returned as test
    /// errors. Panics: none.
    fn path_to_str(path: &Path) -> Result<&str, Box<dyn Error>>
    {
        path.to_str().ok_or_else(|| {
            std::io::Error::other(format!(
                "CLI fixture path must be valid UTF-8: {}",
                path.display()
            ))
            .into()
        })
    }

    /// Converts a test invariant into a fallible test result instead of
    /// panicking.
    ///
    /// # Contract
    /// Preconditions: `message` precisely describes the violated invariant when
    /// `condition` is false. Postconditions: returns `Ok(())` for true
    /// conditions without building the failure message. Failure modes:
    /// false conditions are returned as test errors. Panics: none.
    fn require<F>(
        condition: bool,
        message: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> String,
    {
        if condition {
            return Ok(());
        }

        Err(std::io::Error::other(message()).into())
    }

    /// Verifies that MCP initialize advertises tool support.
    ///
    /// # Contract
    /// Preconditions: Cargo exposes a runnable `aifix` binary. Postconditions:
    /// confirms stdio JSON-RPC initialize succeeds and includes the MCP tools
    /// capability. Failure modes: subprocess, JSON, or capability invariant
    /// errors fail the test. Panics: none.
    #[test]
    fn mcp_initialize_advertises_tools_capability() -> Result<(), Box<dyn Error>>
    {
        let responses = run_mcp(&[mcp_initialize(1)])?;
        let result = mcp_result(mcp_response_by_id(&responses, 1)?)?;

        require(
            result
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("tools"))
                .is_some(),
            || format!("initialize result should advertise tools capability: {result}"),
        )?;
        Ok(())
    }

    /// Verifies that MCP tools/list exposes the diagnostic tools.
    ///
    /// # Contract
    /// Preconditions: Cargo exposes a runnable `aifix` binary. Postconditions:
    /// confirms tools/list contains the MCP tool names required by the public
    /// contract. Failure modes: subprocess, JSON, or missing-name invariant
    /// errors fail the test. Panics: none.
    #[test]
    fn mcp_tools_list_includes_diagnostic_tools() -> Result<(), Box<dyn Error>>
    {
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            json!({
                "jsonrpc": "2.0",
                "id": 2_u64,
                "method": "tools/list"
            }),
        ])?;
        let result = mcp_result(mcp_response_by_id(&responses, 2)?)?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                std::io::Error::other(format!("tools/list result should contain tools: {result}"))
            })?;

        for expected in [
            "aifix_pipeline",
            "aifix_batch",
            "aifix_dedupe",
            "aifix_report_fix",
            "aifix_replay_fixes",
            "aifix_guidance",
        ] {
            require(
                tools
                    .iter()
                    .any(|tool| tool.get("name").and_then(Value::as_str) == Some(expected)),
                || format!("tools/list should include {expected}: {result}"),
            )?;
        }
        Ok(())
    }

    /// Verifies that the MCP pipeline tool renders TypeScript diagnostics.
    ///
    /// # Contract
    /// Preconditions: Cargo exposes a runnable `aifix` binary. Postconditions:
    /// confirms `aifix_pipeline` parses TypeScript text via stdin JSON-RPC and
    /// returns Markdown preserving source code and path. Failure modes:
    /// subprocess, JSON, tool, or text invariant errors fail the test. Panics:
    /// none.
    #[test]
    fn mcp_pipeline_typescript_text_returns_markdown() -> Result<(), Box<dyn Error>>
    {
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            mcp_tool_call(
                2,
                "aifix_pipeline",
                &json!({
                    "input": "src/mcp.ts(7,11): error TS2322: Type 'string' is not assignable to type 'number'.\n",
                    "protocol": "typescript-text",
                    "format": "markdown",
                    "maxDiagnostics": 4_u64
                }),
            ),
        ])?;
        let markdown = mcp_tool_text(mcp_response_by_id(&responses, 2)?)?;

        require(markdown.contains("TS2322"), || {
            format!("MCP pipeline Markdown should preserve TS code: {markdown}")
        })?;
        require(markdown.contains("src/mcp.ts"), || {
            format!("MCP pipeline Markdown should preserve diagnostic path: {markdown}")
        })?;
        require(markdown.contains("not assignable"), || {
            format!("MCP pipeline Markdown should preserve diagnostic message: {markdown}")
        })?;
        Ok(())
    }

    /// Verifies project-local MCP dedupe state and guidance reuse.
    ///
    /// # Contract
    /// Preconditions: the test process can create a temporary project root and
    /// run `aifix mcp`. Postconditions: confirms first-seen diagnostics are
    /// returned, repeated diagnostics are suppressed, and guidance reflects the
    /// observed shape. Failure modes: filesystem, subprocess, JSON, tool, or
    /// text invariant errors fail the test. Panics: none.
    #[test]
    fn mcp_dedupe_suppresses_repeated_diagnostic() -> Result<(), Box<dyn Error>>
    {
        let project_root_dir = create_temp_dir("mcp-dedupe")?;
        let project_root = path_to_str(&project_root_dir)?;
        let diagnostic = json!({
            "source": "tsc",
            "code": "TS7006",
            "severity": "error",
            "message": "Parameter 'value' implicitly has an 'any' type.",
            "spans": [
                {
                    "file": "src/cache.ts",
                    "line": 3_u64,
                    "column": 9_u64
                }
            ]
        });
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            mcp_tool_call(
                2,
                "aifix_dedupe",
                &json!({
                    "projectRoot": project_root,
                    "diagnostics": [diagnostic],
                    "format": "markdown",
                    "maxDiagnostics": 4_u64
                }),
            ),
            mcp_tool_call(
                3,
                "aifix_dedupe",
                &json!({
                    "projectRoot": project_root,
                    "diagnostics": [diagnostic],
                    "format": "markdown",
                    "maxDiagnostics": 4_u64
                }),
            ),
            mcp_tool_call(
                4,
                "aifix_guidance",
                &json!({
                    "projectRoot": project_root,
                    "diagnostics": [diagnostic]
                }),
            ),
        ])?;
        let first = mcp_tool_text(mcp_response_by_id(&responses, 2)?)?;
        let second = mcp_tool_text(mcp_response_by_id(&responses, 3)?)?;
        let guidance = mcp_tool_text(mcp_response_by_id(&responses, 4)?)?;

        require(first.contains("TS7006"), || {
            format!("first dedupe response should include new diagnostic: {first}")
        })?;
        require(first.contains("src/cache.ts"), || {
            format!("first dedupe response should include diagnostic path: {first}")
        })?;
        require(!second.contains("implicitly has an 'any' type"), || {
            format!("second dedupe response should suppress repeated diagnostic: {second}")
        })?;
        require(
            guidance.contains("recurring")
                || guidance.contains("TS7006")
                || guidance.contains("typescript")
                || guidance.contains("src/cache.ts"),
            || format!("guidance should mention recurring shape or source/code: {guidance}"),
        )?;
        Ok(())
    }

    /// Verifies MCP fix reporting can be replayed in suggest mode.
    ///
    /// # Contract
    /// Preconditions: the test process can create a temporary project root and
    /// run `aifix mcp`. Postconditions: confirms a reported diagnostic fix is
    /// returned as suggestion text for the same diagnostic. Failure modes:
    /// filesystem, subprocess, JSON, tool, or text invariant errors fail the
    /// test. Panics: none.
    #[test]
    fn mcp_reported_fix_replays_as_suggestion() -> Result<(), Box<dyn Error>>
    {
        let project_root_dir = create_temp_dir("mcp-fix-cache")?;
        let project_root = path_to_str(&project_root_dir)?;
        let diagnostic = json!({
            "source": "tsc",
            "code": "TS2551",
            "severity": "error",
            "message": "Property 'lenght' does not exist on type 'string'. Did you mean 'length'?",
            "spans": [
                {
                    "file": "src/fix.ts",
                    "line": 1_u64,
                    "column": 9_u64
                }
            ]
        });
        let patch = "\
diff --git a/src/fix.ts b/src/fix.ts
--- a/src/fix.ts
+++ b/src/fix.ts
@@ -1 +1 @@
-value.lenght
+value.length
";
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            mcp_tool_call(
                2,
                "aifix_report_fix",
                &json!({
                    "projectRoot": project_root,
                    "diagnostic": diagnostic,
                    "patch": patch,
                    "note": "Use the canonical string length property."
                }),
            ),
            mcp_tool_call(
                3,
                "aifix_replay_fixes",
                &json!({
                    "projectRoot": project_root,
                    "diagnostics": [diagnostic],
                    "mode": "suggest"
                }),
            ),
        ])?;

        let report_result = mcp_result(mcp_response_by_id(&responses, 2)?)?;
        require(
            report_result.get("isError").and_then(Value::as_bool) != Some(true),
            || format!("report fix should succeed: {report_result}"),
        )?;
        let suggestion = mcp_tool_text(mcp_response_by_id(&responses, 3)?)?;
        require(suggestion.contains("diff --git"), || {
            format!("replay suggestion should include patch header: {suggestion}")
        })?;
        require(suggestion.contains("value.length"), || {
            format!("replay suggestion should include patch body: {suggestion}")
        })?;
        Ok(())
    }

    /// Verifies that clippy compiler-message JSONL becomes a grouped JSON
    /// digest.
    ///
    /// # Contract
    /// Preconditions: the fixture is checked out under this crate's
    /// `tests/fixtures` directory and Cargo exposes the test-built binary.
    /// Postconditions: confirms code, path, message, and count survive JSON
    /// rendering. Failure modes: missing fixture, non-UTF-8 fixture path,
    /// command, UTF-8, JSON, or digest-invariant failures fail the test.
    /// Panics: none.
    #[test]
    fn clippy_json_pipeline_emits_json_digest() -> Result<(), Box<dyn Error>>
    {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clippy.jsonl");
        require(fixture.try_exists()?, || {
            format!(
                "clippy CLI fixture should exist under the crate tests/fixtures directory: {}",
                fixture.display()
            )
        })?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--protocol",
            "clippy-json",
            "--format",
            "json",
            "--input",
            fixture,
            "--max-diagnostics",
            "8",
        ])?;
        let digest = successful_json(output)?;

        require(json_contains_str(&digest, "clippy::unwrap_used"), || {
            format!("digest should preserve the clippy code: {digest}")
        })?;
        require(json_contains_str(&digest, "src/main.rs"), || {
            format!("digest should preserve the diagnostic path: {digest}")
        })?;
        require(json_contains_str(&digest, "used `unwrap()`"), || {
            format!("digest should preserve the diagnostic message: {digest}")
        })?;
        require(json_contains_u64(&digest, 1), || {
            format!(
                "digest should expose at least one count for the single fixture diagnostic: {digest}"
            )
        })?;
        Ok(())
    }

    /// Verifies that TypeScript text diagnostics render as agent-readable
    /// Markdown.
    ///
    /// # Contract
    /// Preconditions: the test process can write a temporary input file and run
    /// the binary. Postconditions: confirms code, path, and message survive
    /// Markdown rendering. Failure modes: command, UTF-8, filesystem, missing
    /// temporary fixture, or Markdown-invariant failures fail the test.
    /// Panics: none.
    #[test]
    fn typescript_text_pipeline_emits_markdown_guidance() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input(
            "typescript",
            "src/app.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\n",
        )?;
        require(input.try_exists()?, || {
            format!(
                "temporary TypeScript CLI fixture should exist before invocation: {}",
                input.display()
            )
        })?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("typescript-text"),
            OsStr::new("--format"),
            OsStr::new("markdown"),
            OsStr::new("--input"),
            input.as_os_str(),
            OsStr::new("--max-diagnostics"),
            OsStr::new("4"),
        ])?;
        let markdown = successful_stdout(output)?;

        require(markdown.contains("TS2322"), || {
            format!("markdown should preserve the TypeScript code: {markdown}")
        })?;
        require(markdown.contains("src/app.ts"), || {
            format!("markdown should preserve the diagnostic path: {markdown}")
        })?;
        require(markdown.contains("not assignable"), || {
            format!("markdown should preserve the diagnostic message: {markdown}")
        })?;
        Ok(())
    }

    /// Verifies that LSP JSON diagnostics can be rendered without raw payload
    /// fields.
    ///
    /// # Contract
    /// Preconditions: the test process can write a temporary input file and run
    /// the binary. Postconditions: confirms compact JSON preserves semantic
    /// fields and omits raw payloads. Failure modes: command, UTF-8, JSON,
    /// filesystem, missing temporary fixture, or compact-digest invariant
    /// failures fail the test. Panics: none.
    #[test]
    fn lsp_json_pipeline_emits_compact_json_digest() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input(
            "lsp",
            r#"{
  "uri": "file:///workspace/src/app.ts",
  "diagnostics": [
    {
      "range": {
        "start": { "line": 2, "character": 10 },
        "end": { "line": 2, "character": 15 }
      },
      "severity": 1,
      "code": "TS2304",
      "source": "typescript",
      "message": "Cannot find name 'Widget'."
    }
  ]
}"#,
        )?;
        require(input.try_exists()?, || {
            format!(
                "temporary LSP CLI fixture should exist before invocation: {}",
                input.display()
            )
        })?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("lsp-json"),
            OsStr::new("--format"),
            OsStr::new("compact-json"),
            OsStr::new("--input"),
            input.as_os_str(),
            OsStr::new("--max-diagnostics"),
            OsStr::new("4"),
        ])?;
        let digest = successful_json(output)?;
        let encoded = digest.to_string();

        require(json_contains_str(&digest, "TS2304"), || {
            format!("compact digest should preserve the LSP code: {digest}")
        })?;
        require(json_contains_str(&digest, "Widget"), || {
            format!("compact digest should preserve the LSP message: {digest}")
        })?;
        require(json_contains_str(&digest, "typescript"), || {
            format!("compact digest should preserve the LSP source: {digest}")
        })?;
        require(!encoded.contains("\"stdout\""), || {
            format!("compact digest should omit raw stdout fields: {encoded}")
        })?;
        require(!encoded.contains("\"stderr\""), || {
            format!("compact digest should omit raw stderr fields: {encoded}")
        })?;
        require(!encoded.contains("\"raw\""), || {
            format!("compact digest should omit raw diagnostic payloads: {encoded}")
        })?;
        Ok(())
    }

    /// Verifies that custom batch mode invokes a real local executable.
    ///
    /// # Contract
    /// Preconditions: `printf` is available on the integration-test platform.
    /// Postconditions: confirms stdout and invocation metadata are preserved in
    /// JSON. Failure modes: command, UTF-8, JSON, or batch-digest invariant
    /// failures fail the test. Panics: none.
    #[test]
    fn custom_batch_command_uses_real_executable() -> Result<(), Box<dyn Error>>
    {
        let output = run_aifix([
            "batch",
            "custom",
            "--protocol",
            "nushell-text",
            "--format",
            "json",
            "--max-diagnostics",
            "1",
            "--",
            "printf",
            "custom batch diagnostic\n",
        ])?;
        let digest = successful_json(output)?;

        require(
            json_contains_str(&digest, "custom batch diagnostic"),
            || format!("batch digest should include printf output: {digest}"),
        )?;
        require(json_contains_str(&digest, "printf"), || {
            format!("batch digest should preserve invocation command metadata: {digest}")
        })?;
        require(json_field_equals_u64(&digest, "exit_code", 0), || {
            format!("batch digest should preserve successful process exit status: {digest}")
        })?;
        Ok(())
    }

    /// Verifies that custom batch capture rejects over-limit stdout.
    ///
    /// # Contract
    /// Preconditions: `yes` is available on the Unix integration-test platform
    /// and terminates when stdout is closed by bounded capture.
    /// Postconditions: confirms oversized stdout is rejected before parsing or
    /// invocation retention.
    /// Failure modes: command, UTF-8, or error-message invariant failures fail
    /// the test. Panics: none.
    #[test]
    fn custom_batch_command_rejects_over_limit_stdout() -> Result<(), Box<dyn Error>>
    {
        let output = run_aifix([
            "batch",
            "custom",
            "--protocol",
            "nushell-text",
            "--format",
            "json",
            "--",
            "yes",
            "aifix over-limit diagnostic",
        ])?;
        let stderr = unsuccessful_stderr(output)?;

        require(
            stderr.contains("stdout from `yes` exceeded capture limit"),
            || format!("stderr should explain bounded stdout rejection: {stderr}"),
        )?;
        Ok(())
    }

    /// Verifies that batch rejects non-UTF-8 extra arguments before execution.
    ///
    /// # Contract
    /// Preconditions: the Unix test process can construct invalid-UTF-8 OS
    /// strings and run the binary. Postconditions: confirms invalid bytes after
    /// `--` are rejected at the CLI boundary and are not lossily converted into
    /// an executable or argument. Failure modes: command, UTF-8, or
    /// error-message invariant failures fail the test. Panics: none.
    #[test]
    fn batch_rejects_non_utf8_extra_args() -> Result<(), Box<dyn Error>>
    {
        let invalid_arg = OsString::from_vec(Vec::from([0x66, 0x80, 0x6f]));
        let output = run_aifix([
            OsStr::new("batch"),
            OsStr::new("custom"),
            OsStr::new("--"),
            invalid_arg.as_os_str(),
        ])?;
        let stderr = unsuccessful_stderr(output)?;

        require(
            stderr.contains("batch extra argument 0 is not valid UTF-8"),
            || format!("stderr should explain non-UTF-8 extra arg rejection: {stderr}"),
        )?;
        Ok(())
    }

    /// Verifies that project config discovery rejects non-file `aifix.toml`.
    ///
    /// # Contract
    /// Preconditions: the test process can create temporary directories and run
    /// the binary. Postconditions: confirms an existing directory named
    /// `aifix.toml` is reported as a configuration error instead of skipped.
    /// Failure modes: filesystem, command, UTF-8, or error-message invariant
    /// failures fail the test. Panics: none.
    #[test]
    fn batch_rejects_non_file_project_config() -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("non-file-config")?;
        let config_path = cwd.join("aifix.toml");
        fs::create_dir_all(&config_path)?;
        let output = run_aifix([
            OsStr::new("batch"),
            OsStr::new("custom"),
            OsStr::new("--cwd"),
            cwd.as_os_str(),
            OsStr::new("--"),
            OsStr::new("printf"),
            OsStr::new("unreachable diagnostic\n"),
        ])?;
        let stderr = unsuccessful_stderr(output)?;

        require(
            stderr.contains("configuration path exists but is not a regular file"),
            || format!("stderr should explain non-file config rejection: {stderr}"),
        )?;
        Ok(())
    }
}
