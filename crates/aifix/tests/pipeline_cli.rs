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
    fn binary_path() -> &'static str
    {
        env!("CARGO_BIN_EXE_aifix")
    }

    /// Runs the binary with the supplied arguments and captures the full
    /// output.
    ///
    /// # Contract
    /// - requires: Cargo exposed a runnable `aifix` binary for this test
    ///   process.
    /// - ensures: returns captured status, stdout, and stderr without
    ///   interpreting the subprocess result.
    /// - fails: returns process-spawn errors to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the subprocess cannot be
    /// spawned.
    fn run_aifix<I, S>(args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(Command::new(binary_path()).args(args).output()?)
    }

    /// Runs the binary with extra environment bindings scoped to the
    /// subprocess.
    ///
    /// # Contract
    /// - requires: Cargo exposed a runnable `aifix` binary for this test
    ///   process.
    /// - ensures: returns captured status, stdout, and stderr without mutating
    ///   the test process environment.
    /// - provides: names in `removed_envs` are hidden from the subprocess
    ///   before names in `envs` are added.
    /// - fails: returns process-spawn errors to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the subprocess cannot be
    /// spawned.
    fn run_aifix_with_env<I, S, E, K, V>(
        args: I,
        envs: E,
        removed_envs: &[&str],
    ) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        E: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut command = Command::new(binary_path());
        command.args(args);
        for name in removed_envs {
            command.env_remove(name);
        }
        command.envs(envs);
        Ok(command.output()?)
    }

    /// Runs the binary with an isolated XDG-style user configuration root.
    fn run_aifix_with_isolated_config<I, S>(
        args: I,
        xdg_config_home: &Path,
    ) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_aifix_with_env(
            args,
            [(OsStr::new("XDG_CONFIG_HOME"), xdg_config_home.as_os_str())],
            &["AIFIX_CONFIG_DIR_MODE", "HOME"],
        )
    }

    /// Captured MCP subprocess transcript used by stdio integration tests.
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
    /// - requires: Cargo exposed a runnable `aifix` binary and `requests`
    ///   contains JSON-RPC objects.
    /// - ensures: writes one JSON request per line, closes stdin, waits for
    ///   process exit, and parses non-empty stdout lines as JSON responses.
    /// - provides: stderr is preserved for assertion diagnostics.
    /// - fails: returns process, stdin, non-zero exit, UTF-8, empty-response,
    ///   or JSON parse errors.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns process, stdin-write, non-zero-exit, UTF-8, empty-response, or
    /// JSON parse errors from the MCP subprocess boundary.
    fn run_mcp(requests: &[Value]) -> Result<McpOutput, Box<dyn Error>>
    {
        run_mcp_with_env(requests, core::iter::empty::<(&OsStr, &OsStr)>(), &[])
    }

    /// Runs the MCP server with scoped environment bindings.
    ///
    /// # Contract
    /// - requires: Cargo exposed a runnable `aifix` binary and `requests`
    ///   contains JSON-RPC objects.
    /// - ensures: applies environment removals and bindings before launching
    ///   the server, then preserves the same transcript guarantees as
    ///   [`run_mcp`].
    /// - fails: returns process, stdin, non-zero exit, UTF-8, empty-response,
    ///   or JSON parse errors.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns process, stdin-write, non-zero-exit, UTF-8, empty-response, or
    /// JSON parse errors from the MCP subprocess boundary.
    fn run_mcp_with_env<I, K, V>(
        requests: &[Value],
        envs: I,
        removed_envs: &[&str],
    ) -> Result<McpOutput, Box<dyn Error>>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut command = Command::new(binary_path());
        command.arg("mcp");
        for name in removed_envs {
            command.env_remove(name);
        }
        command.envs(envs);

        let mut child = command
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

    /// Runs the MCP server with an isolated XDG-style user configuration root.
    fn run_mcp_with_isolated_config(
        requests: &[Value],
        xdg_config_home: &Path,
    ) -> Result<McpOutput, Box<dyn Error>>
    {
        run_mcp_with_env(
            requests,
            [(OsStr::new("XDG_CONFIG_HOME"), xdg_config_home.as_os_str())],
            &["AIFIX_CONFIG_DIR_MODE", "HOME"],
        )
    }

    /// Builds the JSON-RPC initialize request shared by MCP integration tests.
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
    fn mcp_initialized_notification() -> Value
    {
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        })
    }

    /// Builds an MCP `tools/call` request.
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
    /// - requires: `output` came from `run_mcp`.
    /// - ensures: returns the matching response object without cloning it.
    /// - fails: missing response ids include the full response batch and stderr
    ///   in the returned test error.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] when the requested JSON-RPC id is absent.
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
    /// - requires: `response` is a JSON-RPC response from the MCP subprocess.
    /// - ensures: returns the result field only after rejecting protocol
    ///   errors.
    /// - fails: returns JSON-RPC error objects or missing results to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] when the response is a JSON-RPC error or
    /// omits `result`.
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
    /// - requires: `response` is an MCP `tools/call` response.
    /// - ensures: concatenates all text content segments after validating
    ///   `isError` is not true.
    /// - fails: returns tool errors, missing content, or missing text to the
    ///   test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] when the tool reports `isError`, omits
    /// content, or contains no text segments.
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
    /// - requires: `name` is non-empty and filename-safe for the host temp
    ///   directory.
    /// - ensures: returns a process-and-timestamp-scoped path whose contents
    ///   exactly match `contents`.
    /// - fails: returns empty fixture names, system-clock errors, omitted file
    ///   names, or filesystem write errors to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns validation, system-clock, or filesystem write errors from temp
    /// fixture creation.
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
    /// - requires: `name` is non-empty and filename-safe for the host temp
    ///   directory.
    /// - ensures: returns an existing process-and-timestamp-scoped directory.
    /// - fails: returns empty fixture names, system-clock errors, omitted final
    ///   components, or filesystem creation errors to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns validation, system-clock, or filesystem creation errors from
    /// temp directory setup.
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

    /// Writes a minimal Cargo package that `cargo clippy` can inspect without
    /// fetching dependencies.
    fn write_minimal_cargo_package(root: &Path) -> Result<(), Box<dyn Error>>
    {
        fs::write(
            root.join("Cargo.toml"),
            concat!(
                "[package]\n",
                "name = \"aifix-fixture\"\n",
                "version = \"0.0.0\"\n",
                "edition = \"2021\"\n"
            ),
        )?;
        let src = root.join("src");
        fs::create_dir_all(&src)?;
        fs::write(src.join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")?;
        Ok(())
    }

    /// Converts successful command output into a UTF-8 string.
    ///
    /// # Contract
    /// - requires: `output` came from `run_aifix`.
    /// - ensures: returns stdout as UTF-8 only after validating successful CLI
    ///   exit.
    /// - fails: returns unsuccessful exit status or invalid UTF-8 to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] for unsuccessful process status and
    /// [`std::string::FromUtf8Error`] for invalid stdout bytes.
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
    /// - requires: `output` came from `run_aifix`.
    /// - ensures: returns stderr as UTF-8 only after validating failed CLI
    ///   exit.
    /// - fails: returns successful exit status or invalid UTF-8 to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] when the process succeeded and
    /// [`std::string::FromUtf8Error`] for invalid stderr bytes.
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
    /// - requires: `output` came from a command requested to render JSON.
    /// - ensures: returns parsed JSON after validating successful CLI exit and
    ///   non-empty stdout.
    /// - fails: returns unsuccessful exit, empty stdout, invalid UTF-8, or
    ///   invalid JSON to the test.
    /// - panics: none.
    ///
    /// # Errors
    ///
    /// Returns subprocess/status, UTF-8, empty-stdout, or
    /// [`serde_json::Error`] parse failures from JSON CLI output.
    fn successful_json(output: Output) -> Result<Value, Box<dyn Error>>
    {
        let stdout = successful_stdout(output)?;
        require(!stdout.trim_start().is_empty(), || {
            "JSON CLI output should not be empty".to_owned()
        })?;
        Ok(serde_json::from_str(&stdout)?)
    }

    /// Returns true when any JSON string leaf contains `needle`.
    fn json_contains_str(
        value: &Value,
        needle: &str,
    ) -> bool
    {
        if let Some(text) = value.as_str() {
            return text.contains(needle);
        }
        if let Some(items) = value.as_array() {
            return items.iter().any(|item| json_contains_str(item, needle));
        }
        if let Some(fields) = value.as_object() {
            return fields.values().any(|item| json_contains_str(item, needle));
        }
        false
    }

    /// Returns true when any JSON number leaf equals `expected`.
    fn json_contains_u64(
        value: &Value,
        expected: u64,
    ) -> bool
    {
        if let Some(number) = value.as_u64() {
            return number == expected;
        }
        if let Some(items) = value.as_array() {
            return items.iter().any(|item| json_contains_u64(item, expected));
        }
        if let Some(fields) = value.as_object() {
            return fields
                .values()
                .any(|item| json_contains_u64(item, expected));
        }
        false
    }

    /// Returns true when any object field named `field` equals `expected`.
    fn json_field_equals_str(
        value: &Value,
        field: &str,
        expected: &str,
    ) -> bool
    {
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .any(|item| json_field_equals_str(item, field, expected));
        }
        if let Some(fields) = value.as_object() {
            return fields.get(field).and_then(Value::as_str) == Some(expected)
                || fields
                    .values()
                    .any(|item| json_field_equals_str(item, field, expected));
        }
        false
    }

    /// Returns true when any object field named `field` equals `expected`.
    fn json_field_equals_u64(
        value: &Value,
        field: &str,
        expected: u64,
    ) -> bool
    {
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .any(|item| json_field_equals_u64(item, field, expected));
        }
        if let Some(fields) = value.as_object() {
            return fields.get(field).and_then(Value::as_u64) == Some(expected)
                || fields
                    .values()
                    .any(|item| json_field_equals_u64(item, field, expected));
        }
        false
    }

    /// Returns one profile metadata object by stable profile name.
    fn profile_named<'catalog>(
        catalog: &'catalog Value,
        name: &str,
    ) -> Result<&'catalog Value, Box<dyn Error>>
    {
        let profiles = catalog.as_array().ok_or_else(|| {
            std::io::Error::other(format!("profile catalog should be a JSON array: {catalog}"))
        })?;
        profiles
            .iter()
            .find(|profile| profile.get("name").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "profile catalog should include profile {name:?}: {catalog}"
                ))
                .into()
            })
    }

    /// Returns one `batch auto` profile status by stable profile name.
    fn profile_status_named<'digest>(
        digest: &'digest Value,
        name: &str,
    ) -> Result<&'digest Value, Box<dyn Error>>
    {
        let statuses = digest
            .get("profile_statuses")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "auto digest should include profile_statuses array: {digest}"
                ))
            })?;
        statuses
            .iter()
            .find(|status| status.get("profile").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "auto digest should include status for profile {name:?}: {digest}"
                ))
                .into()
            })
    }

    /// Converts a filesystem path into the UTF-8 CLI path expected by `aifix`.
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

    /// Verifies that MCP initialize advertises tool support and agent guidance.
    #[test]
    fn mcp_initialize_advertises_tools_capability_and_instructions() -> Result<(), Box<dyn Error>>
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
        let instructions = result
            .get("instructions")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "initialize result should include string instructions: {result}"
                ))
            })?;
        require(!instructions.trim().is_empty(), || {
            format!("initialize instructions should be non-empty: {result}")
        })?;
        for term in [
            "aifix_pipeline",
            "aifix_batch",
            "aifix_replay_fixes",
            "normalizes diagnostics",
            "does not invent fixes",
            "nonzero",
            "parseable diagnostics",
            "native tools",
        ] {
            require(instructions.contains(term), || {
                format!("initialize instructions should mention {term:?}: {instructions}")
            })?;
        }
        Ok(())
    }

    /// Verifies that MCP tools/list exposes the diagnostic tools.
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

    /// Verifies CLI profile discovery emits machine-readable built-in metadata
    /// and detects a Cargo project shape.
    #[test]
    fn cli_config_profiles_json_lists_builtins_and_detects_rust_fixture()
    -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("cli-profile-catalog-rust")?;
        write_minimal_cargo_package(&cwd)?;
        let xdg_home = create_temp_dir("cli-profile-catalog-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("config"),
                OsStr::new("profiles"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--format"),
                OsStr::new("json"),
            ],
            &xdg_home,
        )?;
        let catalog = successful_json(output)?;

        for expected in ["auto", "rust", "typescript", "agda", "nushell", "custom"] {
            let profile = profile_named(&catalog, expected)?;
            require(
                profile.get("protocol").and_then(Value::as_str).is_some(),
                || format!("profile {expected:?} should include a protocol: {profile}"),
            )?;
            require(
                profile
                    .get("command_family")
                    .and_then(Value::as_str)
                    .is_some(),
                || format!("profile {expected:?} should include command metadata: {profile}"),
            )?;
        }

        let rust = profile_named(&catalog, "rust")?;
        require(
            rust.get("detected").and_then(Value::as_bool) == Some(true),
            || format!("rust profile should be detected for Cargo.toml fixture: {catalog}"),
        )?;
        require(
            rust.get("detection_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| reason.contains("Cargo.toml")),
            || format!("rust detection should explain the Cargo.toml marker: {rust}"),
        )?;
        Ok(())
    }

    /// Verifies unknown CLI batch profiles fail with valid-profile recovery
    /// details instead of falling through to command execution.
    #[test]
    fn cli_batch_unknown_profile_lists_available_profiles_and_recovery()
    -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("cli-unknown-profile")?;
        let xdg_home = create_temp_dir("cli-unknown-profile-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("batch"),
                OsStr::new("cargo-check"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
            ],
            &xdg_home,
        )?;
        let stderr = unsuccessful_stderr(output)?;

        for expected in [
            "cargo-check",
            "auto",
            "rust",
            "typescript",
            "agda",
            "nushell",
            "custom",
            "aifix config profiles --format json",
        ] {
            require(stderr.contains(expected), || {
                format!("unknown profile stderr should mention {expected:?}: {stderr}")
            })?;
        }
        Ok(())
    }

    /// Verifies MCP profile discovery returns parseable catalog JSON with
    /// built-in names and detection fields.
    #[test]
    fn mcp_batch_profiles_json_lists_builtins_and_detection_fields() -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("mcp-profile-catalog-rust")?;
        write_minimal_cargo_package(&cwd)?;
        let cwd = path_to_str(&cwd)?;
        let xdg_home = create_temp_dir("mcp-profile-catalog-xdg")?;
        let responses = run_mcp_with_isolated_config(
            &[
                mcp_initialize(1),
                mcp_initialized_notification(),
                mcp_tool_call(
                    2,
                    "aifix_batch_profiles",
                    &json!({
                        "cwd": cwd,
                        "format": "json"
                    }),
                ),
            ],
            &xdg_home,
        )?;
        let text = mcp_tool_text(mcp_response_by_id(&responses, 2)?)?;
        let catalog: Value = serde_json::from_str(&text)?;

        for expected in ["auto", "rust", "typescript", "agda", "nushell", "custom"] {
            let profile = profile_named(&catalog, expected)?;
            require(
                profile.get("detected").and_then(Value::as_bool).is_some(),
                || format!("profile {expected:?} should include detected bool: {profile}"),
            )?;
            require(
                profile
                    .get("detection_reason")
                    .and_then(Value::as_str)
                    .is_some(),
                || format!("profile {expected:?} should include detection reason: {profile}"),
            )?;
        }
        require(
            profile_named(&catalog, "rust")?
                .get("detected")
                .and_then(Value::as_bool)
                == Some(true),
            || format!("rust profile should be detected for MCP Cargo fixture: {catalog}"),
        )?;
        Ok(())
    }

    /// Verifies MCP unknown-profile recovery remains a protocol-successful tool
    /// error with structured machine-readable metadata.
    #[test]
    fn mcp_batch_unknown_profile_returns_structured_tool_error() -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("mcp-unknown-profile")?;
        let cwd = path_to_str(&cwd)?;
        let xdg_home = create_temp_dir("mcp-unknown-profile-xdg")?;
        let responses = run_mcp_with_isolated_config(
            &[
                mcp_initialize(1),
                mcp_initialized_notification(),
                mcp_tool_call(
                    2,
                    "aifix_batch",
                    &json!({
                        "cwd": cwd,
                        "profile": "cargo-check",
                        "format": "json"
                    }),
                ),
            ],
            &xdg_home,
        )?;
        let result = mcp_result(mcp_response_by_id(&responses, 2)?)?;
        require(
            result.get("isError").and_then(Value::as_bool) == Some(true),
            || format!("unknown profile should be a tool-level error: {result}"),
        )?;
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "structured tool error should include text content: {result}"
                ))
            })?;
        require(text.contains("cargo-check"), || {
            format!("unknown profile text should mention rejected profile: {text}")
        })?;

        let structured = result.get("structuredContent").ok_or_else(|| {
            std::io::Error::other(format!(
                "unknown profile error should include structuredContent: {result}"
            ))
        })?;
        require(
            structured.get("kind").and_then(Value::as_str) == Some("unknown-profile"),
            || format!("structured error should name unknown-profile kind: {structured}"),
        )?;
        require(
            structured.get("profile").and_then(Value::as_str) == Some("cargo-check"),
            || format!("structured error should echo rejected profile: {structured}"),
        )?;
        let available = structured
            .get("available_profiles")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "structured error should include available_profiles array: {structured}"
                ))
            })?;
        for expected in ["auto", "rust", "typescript", "agda", "nushell", "custom"] {
            require(
                available
                    .iter()
                    .any(|profile| profile.as_str() == Some(expected)),
                || format!("available_profiles should include {expected:?}: {structured}"),
            )?;
        }
        require(
            structured
                .get("recovery_hint")
                .and_then(Value::as_str)
                .is_some_and(|hint| {
                    hint.contains("aifix_batch_profiles")
                        && hint.contains("aifix config profiles --format json")
                }),
            || format!("structured error should include actionable recovery hint: {structured}"),
        )?;
        Ok(())
    }

    /// Verifies `auto` rejects profile-specific extra arguments with actionable
    /// recovery text.
    #[test]
    fn cli_batch_auto_rejects_extra_args_with_profile_specific_recovery()
    -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("cli-auto-extra-args")?;
        let xdg_home = create_temp_dir("cli-auto-extra-args-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("batch"),
                OsStr::new("auto"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--"),
                OsStr::new("unexpected"),
            ],
            &xdg_home,
        )?;
        let stderr = unsuccessful_stderr(output)?;

        for expected in [
            "auto",
            "extra arguments are profile-specific",
            "Use a named profile",
            "aifix config profiles --format json",
        ] {
            require(stderr.contains(expected), || {
                format!("auto extra-args stderr should mention {expected:?}: {stderr}")
            })?;
        }
        Ok(())
    }

    /// Verifies that the MCP pipeline tool renders TypeScript diagnostics.
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

    /// Verifies MCP fix reporting can be replayed in suggest mode with audit
    /// JSON.
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
        let replay: Value = serde_json::from_str(&suggestion)?;
        require(suggestion.contains("diff --git"), || {
            format!("replay suggestion should include patch header: {suggestion}")
        })?;
        require(suggestion.contains("value.length"), || {
            format!("replay suggestion should include patch body: {suggestion}")
        })?;
        require(
            replay
                .pointer("/result/matches/0/fix/patch")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("value.length")),
            || format!("replay JSON matches should preserve patch text: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/confidence")
                .and_then(Value::as_str)
                == Some("exact"),
            || format!("replay audit should report exact confidence: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/matched_signature")
                .and_then(Value::as_str)
                .is_some(),
            || format!("replay audit should include matched signature: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/git_check_ran")
                .and_then(Value::as_bool)
                == Some(false),
            || format!("suggest replay audit should not run git check: {replay}"),
        )?;
        Ok(())
    }

    /// Verifies clippy-json diagnostics round-trip through report-fix and
    /// replay.
    #[test]
    fn mcp_clippy_json_report_fix_replays_for_present_target() -> Result<(), Box<dyn Error>>
    {
        let project_root_dir = create_temp_dir("mcp-clippy-fix-cache")?;
        let src_dir = project_root_dir.join("src");
        fs::create_dir_all(&src_dir)?;
        fs::write(
            src_dir.join("main.rs"),
            "fn main() {\n    let maybe = Some(1);\n    let _a = 0;\n    let _b = 0;\n    let _c = 0;\n    let _d = 0;\n    let value = maybe.unwrap();\n}\n",
        )?;
        let project_root = path_to_str(&project_root_dir)?;
        let diagnostic = json!({
            "source": "clippy",
            "code": "clippy::unwrap_used",
            "severity": "warning",
            "message": "used `unwrap()` on an `Option` value",
            "spans": [
                {
                    "file": "src/main.rs",
                    "line": 7_u64,
                    "column": 18_u64,
                    "end_line": 7_u64,
                    "end_column": 31_u64
                }
            ]
        });
        let clippy_json = r#"{"reason":"compiler-message","message":{"message":"used `unwrap()` on an `Option` value","level":"warning","code":{"code":"clippy::unwrap_used"},"spans":[{"file_name":"src/main.rs","line_start":7,"column_start":18,"line_end":7,"column_end":31,"is_primary":true}]}}"#;
        let patch = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -7 +7 @@
-    let value = maybe.unwrap();
+    let value = maybe.expect(\"present\");
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
                    "patch": patch
                }),
            ),
            mcp_tool_call(
                3,
                "aifix_replay_fixes",
                &json!({
                    "projectRoot": project_root,
                    "input": clippy_json,
                    "protocol": "clippy-json",
                    "mode": "suggest"
                }),
            ),
        ])?;

        let report_result = mcp_result(mcp_response_by_id(&responses, 2)?)?;
        require(
            report_result.get("isError").and_then(Value::as_bool) != Some(true),
            || format!("report fix should succeed: {report_result}"),
        )?;
        let replay_text = mcp_tool_text(mcp_response_by_id(&responses, 3)?)?;
        let replay: Value = serde_json::from_str(&replay_text)?;
        require(
            replay
                .pointer("/result/diagnostics/0/confidence")
                .and_then(Value::as_str)
                == Some("exact"),
            || format!("clippy replay audit should report exact confidence: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/matches/0/fix/patch")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("maybe.expect")),
            || format!("clippy replay should preserve exact-line git patch text: {replay}"),
        )?;
        Ok(())
    }

    /// Verifies MCP replay emits a no-match audit entry for unmatched
    /// diagnostics.
    #[test]
    fn mcp_replay_fixes_reports_no_match_audit() -> Result<(), Box<dyn Error>>
    {
        let project_root_dir = create_temp_dir("mcp-fix-no-match")?;
        let project_root = path_to_str(&project_root_dir)?;
        let diagnostic = json!({
            "source": "tsc",
            "code": "TS2304",
            "severity": "error",
            "message": "Cannot find name 'missingValue'.",
            "spans": [
                {
                    "file": "src/no-match.ts",
                    "line": 2_u64,
                    "column": 13_u64
                }
            ]
        });
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            mcp_tool_call(
                2,
                "aifix_replay_fixes",
                &json!({
                    "projectRoot": project_root,
                    "diagnostics": [diagnostic],
                    "mode": "suggest"
                }),
            ),
        ])?;

        let replay_text = mcp_tool_text(mcp_response_by_id(&responses, 2)?)?;
        let replay: Value = serde_json::from_str(&replay_text)?;
        require(
            replay
                .pointer("/result/matches")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("no-match replay should not return cached matches: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/confidence")
                .and_then(Value::as_str)
                == Some("no-match"),
            || format!("replay audit should report no-match confidence: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/matched_signature")
                .is_none(),
            || format!("no-match replay audit should omit matched signature: {replay}"),
        )?;
        require(
            replay
                .pointer("/result/diagnostics/0/git_check_ran")
                .and_then(Value::as_bool)
                == Some(false),
            || format!("no-match suggest replay should not run git check: {replay}"),
        )?;
        Ok(())
    }

    /// Verifies that clippy compiler-message JSONL becomes a grouped JSON
    /// digest.
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

    /// Verifies that Agda CLI text diagnostics become a grouped JSON digest.
    #[test]
    fn agda_text_pipeline_emits_json_digest() -> Result<(), Box<dyn Error>>
    {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agda.txt");
        require(fixture.try_exists()?, || {
            format!(
                "Agda CLI fixture should exist under the crate tests/fixtures directory: {}",
                fixture.display()
            )
        })?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
            "--max-diagnostics",
            "4",
        ])?;
        let digest = successful_json(output)?;

        require(
            json_field_equals_str(&digest, "code", "UnequalSorts"),
            || format!("Agda digest should preserve the Agda code tag: {digest}"),
        )?;
        require(json_field_equals_str(&digest, "source", "agda"), || {
            format!("Agda digest should preserve the source name: {digest}")
        })?;
        require(
            json_field_equals_str(&digest, "file", "/workspace/agda/Bad.agda"),
            || format!("Agda digest should preserve the diagnostic path: {digest}"),
        )?;
        require(
            json_contains_str(&digest, "expression Set has type Set"),
            || format!("Agda digest should preserve the diagnostic message body: {digest}"),
        )?;
        Ok(())
    }

    /// Verifies that Agda multi-line source spans preserve both endpoints.
    #[test]
    fn agda_text_pipeline_preserves_multiline_span() -> Result<(), Box<dyn Error>>
    {
        let temp_dir = create_temp_dir("agda-multiline-span")?;
        let fixture = temp_dir.join("agda.txt");
        fs::write(
            &fixture,
            concat!(
                "Checking Core.Polygraph (/workspace/agda/Polygraph.agda).\n",
                "/workspace/agda/Polygraph.agda:138.49-386.26: error: ",
                "[UnsolvedInteractionMetas]\n",
                "Unsolved interaction metas at the following locations:\n",
                "  /workspace/agda/Polygraph.agda:138.49-53\n",
            ),
        )?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
        ])?;
        let digest = successful_json(output)?;
        let span = digest.pointer("/diagnostics/0/spans/0").ok_or_else(|| {
            std::io::Error::other(format!(
                "Agda digest should contain a primary span: {digest}"
            ))
        })?;

        require(
            json_field_equals_str(&digest, "code", "UnsolvedInteractionMetas"),
            || format!("Agda digest should preserve the multiline diagnostic code: {digest}"),
        )?;
        require(
            span.get("line").and_then(Value::as_u64) == Some(138),
            || format!("Agda span should preserve start line 138: {span}"),
        )?;
        require(
            span.get("column").and_then(Value::as_u64) == Some(49),
            || format!("Agda span should preserve start column 49: {span}"),
        )?;
        require(
            span.get("end_line").and_then(Value::as_u64) == Some(386),
            || format!("Agda span should preserve end line 386: {span}"),
        )?;
        require(
            span.get("end_column").and_then(Value::as_u64) == Some(26),
            || format!("Agda span should preserve end column 26: {span}"),
        )?;
        Ok(())
    }

    /// Verifies that status-only successful Agda output is accepted as clean.
    #[test]
    fn agda_text_pipeline_accepts_status_only_output() -> Result<(), Box<dyn Error>>
    {
        let temp_dir = create_temp_dir("agda-status-only")?;
        let fixture = temp_dir.join("agda-status.txt");
        fs::write(
            &fixture,
            "Checking Internal.Everything (/workspace/agda/Everything.agda).\nFinished Internal.Everything.\n",
        )?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
            "--fail-on-diagnostics",
        ])?;
        let digest = successful_json(output)?;

        require(
            digest.pointer("/counts/total").and_then(Value::as_u64) == Some(0),
            || format!("status-only Agda output should have zero diagnostics: {digest}"),
        )?;
        require(
            digest
                .pointer("/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("status-only Agda output should retain no diagnostics: {digest}"),
        )?;
        require(
            digest
                .pointer("/groups")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("status-only Agda output should retain no groups: {digest}"),
        )?;
        Ok(())
    }

    /// Verifies that default protocol detection treats Agda progress as clean
    /// status.
    #[test]
    fn agda_auto_pipeline_accepts_status_only_output() -> Result<(), Box<dyn Error>>
    {
        let temp_dir = create_temp_dir("agda-auto-status-only")?;
        let fixture = temp_dir.join("agda-auto-status.txt");
        fs::write(
            &fixture,
            "Checking Internal.Everything (/workspace/agda/Everything.agda).\nFinished Internal.Everything.\n",
        )?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--format",
            "json",
            "--input",
            fixture,
            "--fail-on-diagnostics",
        ])?;
        let stdout = successful_stdout(output)?;
        let digest: Value = serde_json::from_str(&stdout)?;

        require(
            digest.pointer("/counts/total").and_then(Value::as_u64) == Some(0),
            || format!("status-only Agda progress should have zero diagnostics: {digest}"),
        )?;
        require(
            digest
                .pointer("/diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("status-only Agda progress should render no diagnostics: {digest}"),
        )?;
        require(
            digest
                .pointer("/groups")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("status-only Agda progress should render no groups: {digest}"),
        )?;
        require(!stdout.contains("nushell"), || {
            format!("status-only Agda progress should not render Nushell diagnostics: {stdout}")
        })?;
        Ok(())
    }

    /// Verifies that expected diagnostic codes make CLI gate mode selective.
    #[test]
    fn agda_pipeline_gate_honors_expected_codes() -> Result<(), Box<dyn Error>>
    {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agda.txt");
        let fixture = path_to_str(&fixture)?;

        let failing = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
            "--fail-on-diagnostics",
        ])?;
        require(!failing.status.success(), || {
            format!(
                "Agda gate should fail without expected codes; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&failing.stdout),
                String::from_utf8_lossy(&failing.stderr)
            )
        })?;
        let failing_digest: Value = serde_json::from_slice(&failing.stdout)?;
        require(json_contains_str(&failing_digest, "UnequalSorts"), || {
            format!("failing Agda gate should still render diagnostics: {failing_digest}")
        })?;
        require(
            String::from_utf8_lossy(&failing.stderr).contains("unexpected diagnostics"),
            || {
                format!(
                    "failing Agda gate should explain the gate failure: {}",
                    String::from_utf8_lossy(&failing.stderr)
                )
            },
        )?;

        let expected = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
            "--fail-on-diagnostics",
            "--expected-code",
            "UnequalSorts",
            "--expected-code",
            "FileNotFound",
        ])?;
        let expected_digest = successful_json(expected)?;
        require(json_contains_str(&expected_digest, "UnequalSorts"), || {
            format!("expected-code gate should keep allowed diagnostics visible: {expected_digest}")
        })?;

        let alias = run_aifix([
            "pipeline",
            "--protocol",
            "agda-text",
            "--format",
            "json",
            "--input",
            fixture,
            "--fail-on-diagnostics",
            "--allow-code",
            "UnequalSorts",
            "--allow-code",
            "FileNotFound",
        ])?;
        let alias_digest = successful_json(alias)?;
        require(json_contains_str(&alias_digest, "FileNotFound"), || {
            format!("allow-code alias gate should keep allowed diagnostics visible: {alias_digest}")
        })?;
        Ok(())
    }

    /// Verifies that auto protocol detection recognizes Agda text diagnostics.
    #[test]
    fn auto_pipeline_detects_agda_text_before_generic_nushell() -> Result<(), Box<dyn Error>>
    {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agda.txt");
        require(fixture.try_exists()?, || {
            format!(
                "Agda CLI fixture should exist under the crate tests/fixtures directory: {}",
                fixture.display()
            )
        })?;
        let fixture = path_to_str(&fixture)?;
        let output = run_aifix([
            "pipeline",
            "--protocol",
            "auto",
            "--format",
            "compact-json",
            "--input",
            fixture,
            "--max-diagnostics",
            "4",
        ])?;
        let digest = successful_json(output)?;
        let encoded = digest.to_string();

        require(
            json_field_equals_str(&digest, "code", "UnequalSorts"),
            || format!("auto Agda digest should preserve the Agda code tag: {digest}"),
        )?;
        require(json_field_equals_str(&digest, "source", "agda"), || {
            format!("auto Agda digest should preserve the Agda source: {digest}")
        })?;
        require(!encoded.contains("nushell"), || {
            format!("auto Agda digest should not fall back to generic Nushell text: {encoded}")
        })?;
        Ok(())
    }

    /// Verifies that the default config path policy honors `XDG_CONFIG_HOME`.
    #[test]
    fn config_paths_reports_xdg_user_config_by_default() -> Result<(), Box<dyn Error>>
    {
        let xdg_home = create_temp_dir("xdg-config-paths")?;
        let expected = xdg_home.join("aifix").join("aifix.toml");
        let output = run_aifix_with_env(
            [OsStr::new("config"), OsStr::new("paths")],
            [(OsStr::new("XDG_CONFIG_HOME"), xdg_home.as_os_str())],
            &["AIFIX_CONFIG_DIR_MODE"],
        )?;
        let stdout = successful_stdout(output)?;
        let expected = path_to_str(&expected)?;
        let expected_line = format!("user: {expected}");

        require(
            stdout.lines().any(|line| line == expected_line.as_str()),
            || format!("config paths should report the default XDG user path {expected}: {stdout}"),
        )?;
        Ok(())
    }

    /// Verifies that pipeline defaults load from the XDG user config path.
    #[test]
    fn pipeline_loads_defaults_from_xdg_user_config() -> Result<(), Box<dyn Error>>
    {
        let xdg_home = create_temp_dir("xdg-config-loading")?;
        let config_dir = xdg_home.join("aifix");
        fs::create_dir_all(&config_dir)?;
        fs::write(
            config_dir.join("aifix.toml"),
            "default_protocol = \"clippy-json\"\ndefault_format = \"compact-json\"\n",
        )?;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clippy.jsonl");
        require(fixture.try_exists()?, || {
            format!(
                "clippy CLI fixture should exist under the crate tests/fixtures directory: {}",
                fixture.display()
            )
        })?;
        let output = run_aifix_with_env(
            [
                OsStr::new("pipeline"),
                OsStr::new("--input"),
                fixture.as_os_str(),
            ],
            [(OsStr::new("XDG_CONFIG_HOME"), xdg_home.as_os_str())],
            &["AIFIX_CONFIG_DIR_MODE"],
        )?;
        let digest = successful_json(output)?;

        require(json_contains_str(&digest, "clippy::unwrap_used"), || {
            format!("XDG user config defaults should parse the clippy fixture: {digest}")
        })?;
        require(json_contains_str(&digest, "src/main.rs"), || {
            format!("XDG user config defaults should preserve diagnostic paths: {digest}")
        })?;
        Ok(())
    }

    /// Verifies that platform-native mode opts out of the default XDG path.
    #[test]
    fn platform_native_config_dir_mode_omits_default_xdg_user_path() -> Result<(), Box<dyn Error>>
    {
        let xdg_home = create_temp_dir("xdg-config-native-mode")?;
        let default_xdg_path = xdg_home.join("aifix").join("aifix.toml");
        let output = run_aifix_with_env(
            [OsStr::new("config"), OsStr::new("paths")],
            [
                (OsStr::new("XDG_CONFIG_HOME"), xdg_home.as_os_str()),
                (
                    OsStr::new("AIFIX_CONFIG_DIR_MODE"),
                    OsStr::new("platform-native"),
                ),
            ],
            &[],
        )?;
        let stdout = successful_stdout(output)?;
        let default_xdg_path = path_to_str(&default_xdg_path)?;

        require(!stdout.contains(default_xdg_path), || {
            format!(
                "platform-native mode should not report the default XDG user path {default_xdg_path}: {stdout}"
            )
        })?;
        Ok(())
    }

    /// Verifies that unsupported config directory modes fail explicitly.
    #[test]
    fn unsupported_config_dir_mode_exits_with_config_error() -> Result<(), Box<dyn Error>>
    {
        let output = run_aifix_with_env(
            [OsStr::new("config"), OsStr::new("paths")],
            [(
                OsStr::new("AIFIX_CONFIG_DIR_MODE"),
                OsStr::new("space-cadet"),
            )],
            &[],
        )?;
        let stderr = unsuccessful_stderr(output)?;
        let lower_stderr = stderr.to_ascii_lowercase();

        require(lower_stderr.contains("config"), || {
            format!("unsupported mode error should identify configuration: {stderr}")
        })?;
        require(stderr.contains("AIFIX_CONFIG_DIR_MODE"), || {
            format!("unsupported mode error should name AIFIX_CONFIG_DIR_MODE: {stderr}")
        })?;
        require(stderr.contains("space-cadet"), || {
            format!("unsupported mode error should echo the unsupported value: {stderr}")
        })?;
        Ok(())
    }

    /// Verifies that TypeScript text diagnostics render as agent-readable
    /// Markdown.
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

    /// Verifies that empty auto pipeline input is an explicit zero-diagnostic
    /// digest.
    #[test]
    fn auto_pipeline_empty_input_emits_zero_diagnostics() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input("empty-auto", "")?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("auto"),
            OsStr::new("--format"),
            OsStr::new("json"),
            OsStr::new("--input"),
            input.as_os_str(),
        ])?;
        let digest = successful_json(output)?;

        require(
            digest.pointer("/counts/total").and_then(Value::as_u64) == Some(0),
            || format!("empty auto pipeline should report zero total diagnostics: {digest}"),
        )?;
        require(
            digest
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("empty auto pipeline should include an empty diagnostics array: {digest}"),
        )?;
        require(
            digest
                .get("groups")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("empty auto pipeline should include an empty groups array: {digest}"),
        )?;
        Ok(())
    }

    /// Verifies that MCP pipeline results preserve empty auto input as an
    /// explicit zero-diagnostic digest.
    #[test]
    fn mcp_pipeline_empty_input_returns_zero_diagnostics() -> Result<(), Box<dyn Error>>
    {
        let responses = run_mcp(&[
            mcp_initialize(1),
            mcp_initialized_notification(),
            mcp_tool_call(
                2,
                "aifix_pipeline",
                &json!({
                    "input": "",
                    "protocol": "auto",
                    "format": "json"
                }),
            ),
        ])?;
        let text = mcp_tool_text(mcp_response_by_id(&responses, 2)?)?;
        let digest: Value = serde_json::from_str(&text)?;

        require(
            digest.pointer("/counts/total").and_then(Value::as_u64) == Some(0),
            || format!("empty MCP pipeline should report zero total diagnostics: {digest}"),
        )?;
        require(
            digest
                .get("diagnostics")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty),
            || format!("empty MCP pipeline should include an empty diagnostics array: {digest}"),
        )?;
        Ok(())
    }

    /// Verifies that noisy cargo streams retain valid compiler diagnostics.
    #[test]
    fn noisy_cargo_pipeline_retains_good_diagnostics() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input(
            "noisy-cargo",
            concat!(
                "{\"reason\":\"compiler-artifact\",\"package_id\":\"demo 0.1.0\"}\n",
                "warning: build script emitted non-json noise\n",
                "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"used `unwrap()` on an `Option` value\",\"level\":\"warning\",\"code\":{\"code\":\"clippy::unwrap_used\"},\"spans\":[{\"file_name\":\"src/main.rs\",\"line_start\":7,\"column_start\":9,\"line_end\":7,\"column_end\":15,\"is_primary\":true}]}}\n",
                "{\"reason\":\"compiler-message\",\"message\":"
            ),
        )?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("auto"),
            OsStr::new("--format"),
            OsStr::new("json"),
            OsStr::new("--input"),
            input.as_os_str(),
        ])?;
        let digest = successful_json(output)?;

        require(
            digest.pointer("/counts/total").and_then(Value::as_u64) == Some(1),
            || format!("noisy cargo pipeline should retain the one good diagnostic: {digest}"),
        )?;
        require(json_contains_str(&digest, "clippy::unwrap_used"), || {
            format!("noisy cargo pipeline should preserve the clippy code: {digest}")
        })?;
        require(json_contains_str(&digest, "src/main.rs"), || {
            format!("noisy cargo pipeline should preserve the diagnostic path: {digest}")
        })?;
        Ok(())
    }

    /// Verifies that malformed-only cargo-shaped structured input is rejected.
    #[test]
    fn malformed_only_cargo_pipeline_is_rejected() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input(
            "malformed-cargo",
            "{\"reason\":\"compiler-message\",\"message\":{\"message\":\"truncated\"",
        )?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("auto"),
            OsStr::new("--format"),
            OsStr::new("json"),
            OsStr::new("--input"),
            input.as_os_str(),
        ])?;
        let stderr = unsuccessful_stderr(output)?;

        require(stderr.contains("json error"), || {
            format!("malformed-only cargo pipeline should report a JSON error: {stderr}")
        })?;
        Ok(())
    }

    /// Verifies that maxDiagnostics hides only samples, not counts.
    #[test]
    fn markdown_max_diagnostics_reports_hidden_samples() -> Result<(), Box<dyn Error>>
    {
        let input = write_temp_input(
            "max-diagnostics-markdown",
            concat!(
                "src/app.ts(1,1): error TS2322: Type 'string' is not assignable to type 'number'.\n",
                "src/app.ts(2,1): error TS2322: Type 'string' is not assignable to type 'number'.\n",
                "src/app.ts(3,1): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            ),
        )?;
        let output = run_aifix([
            OsStr::new("pipeline"),
            OsStr::new("--protocol"),
            OsStr::new("typescript-text"),
            OsStr::new("--format"),
            OsStr::new("markdown"),
            OsStr::new("--input"),
            input.as_os_str(),
            OsStr::new("--max-diagnostics"),
            OsStr::new("1"),
        ])?;
        let markdown = successful_stdout(output)?;

        require(markdown.contains("- Total diagnostics: 3"), || {
            format!("Markdown should preserve full diagnostic totals: {markdown}")
        })?;
        require(markdown.contains("- Count: 3"), || {
            format!("Markdown should preserve full group counts: {markdown}")
        })?;
        require(
            markdown.contains("- Hidden samples: 2 (retained 1 of 3 diagnostics"),
            || format!("Markdown should report maxDiagnostics sample truncation: {markdown}"),
        )?;
        Ok(())
    }

    /// Verifies that LSP JSON diagnostics can be rendered without raw payload
    /// fields.
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

    /// Verifies `batch auto` runs only the detected Rust built-in for a tiny
    /// Cargo fixture and reports skipped non-Rust built-ins instead of failing.
    #[test]
    fn auto_batch_rust_fixture_reports_rust_ran_and_non_rust_skipped() -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("auto-rust-only")?;
        write_minimal_cargo_package(&cwd)?;
        let xdg_home = create_temp_dir("auto-rust-only-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("batch"),
                OsStr::new("auto"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--format"),
                OsStr::new("json"),
                OsStr::new("--max-diagnostics"),
                OsStr::new("1"),
            ],
            &xdg_home,
        )?;
        let digest = successful_json(output)?;

        let rust = profile_status_named(&digest, "rust")?;
        require(
            rust.get("state").and_then(Value::as_str) == Some("ran"),
            || format!("rust profile should run for Cargo fixture: {digest}"),
        )?;
        require(
            rust.get("diagnostic_count")
                .and_then(Value::as_u64)
                .is_some(),
            || format!("ran rust profile should report diagnostic_count: {rust}"),
        )?;

        for expected in ["typescript", "agda", "nushell"] {
            let status = profile_status_named(&digest, expected)?;
            require(
                status.get("state").and_then(Value::as_str) == Some("skipped"),
                || format!("non-Rust profile {expected:?} should be skipped: {digest}"),
            )?;
            require(
                status.get("reason").and_then(Value::as_str).is_some(),
                || format!("skipped profile {expected:?} should explain detection: {status}"),
            )?;
        }
        Ok(())
    }

    /// Verifies `batch auto` reports both Rust and TypeScript statuses for a
    /// mixed-shape fixture, while tolerating systems without `tsc`.
    #[test]
    fn auto_batch_mixed_rust_typescript_fixture_reports_both_statuses() -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("auto-rust-typescript")?;
        write_minimal_cargo_package(&cwd)?;
        fs::write(
            cwd.join("tsconfig.json"),
            r#"{"compilerOptions":{"noEmit":true,"strict":true},"include":["src/**/*.ts"]}"#,
        )?;
        fs::write(
            cwd.join("src").join("index.ts"),
            "const value: number = 1;\n",
        )?;
        let xdg_home = create_temp_dir("auto-rust-typescript-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("batch"),
                OsStr::new("auto"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--format"),
                OsStr::new("json"),
                OsStr::new("--max-diagnostics"),
                OsStr::new("1"),
            ],
            &xdg_home,
        )?;
        let digest = successful_json(output)?;

        require(
            profile_status_named(&digest, "rust")?
                .get("state")
                .and_then(Value::as_str)
                == Some("ran"),
            || format!("rust profile should run for mixed Cargo fixture: {digest}"),
        )?;
        let typescript = profile_status_named(&digest, "typescript")?;
        let typescript_state =
            typescript
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "typescript status should include machine-readable state: {typescript}"
                    ))
                })?;
        require(matches!(typescript_state, "ran" | "failed"), || {
            format!("typescript should be selected, not skipped, for tsconfig fixture: {digest}")
        })?;
        require(
            typescript.get("protocol").and_then(Value::as_str).is_some()
                && typescript
                    .get("command_family")
                    .and_then(Value::as_str)
                    .is_some(),
            || format!("typescript status should report protocol and command family: {typescript}"),
        )?;
        match typescript_state {
            | "ran" => {
                require(
                    typescript
                        .get("diagnostic_count")
                        .and_then(Value::as_u64)
                        .is_some(),
                    || {
                        format!(
                            "ran TypeScript status should report diagnostic_count: {typescript}"
                        )
                    },
                )?;
            },
            | "failed" => {
                require(
                    typescript
                        .get("error_kind")
                        .and_then(Value::as_str)
                        .is_some(),
                    || format!("failed TypeScript status should report error_kind: {typescript}"),
                )?;
            },
            | _ => {
                return Err(std::io::Error::other(format!(
                    "typescript state should be ran or failed after validation: {typescript_state}"
                ))
                .into());
            },
        }
        Ok(())
    }

    /// Verifies configured `auto = true` profiles preserve diagnostics from a
    /// successful profile while reporting another profile's spawn failure.
    #[test]
    fn auto_batch_configured_profiles_keep_partial_diagnostics_and_failure_status()
    -> Result<(), Box<dyn Error>>
    {
        let cwd = create_temp_dir("auto-configured-partial-failure")?;
        fs::write(
            cwd.join("aifix.toml"),
            concat!(
                "[profiles.good_auto]\n",
                "argv = [\"printf\", \"partial auto diagnostic\\n\"]\n",
                "protocol = \"nushell-text\"\n",
                "auto = true\n\n",
                "[profiles.bad_auto]\n",
                "argv = [\"aifix-impossible-executable-for-auto-profile\"]\n",
                "protocol = \"nushell-text\"\n",
                "auto = true\n",
            ),
        )?;
        let xdg_home = create_temp_dir("auto-configured-partial-failure-xdg")?;
        let output = run_aifix_with_isolated_config(
            [
                OsStr::new("batch"),
                OsStr::new("auto"),
                OsStr::new("--cwd"),
                cwd.as_os_str(),
                OsStr::new("--format"),
                OsStr::new("json"),
            ],
            &xdg_home,
        )?;
        let digest = successful_json(output)?;

        require(
            digest
                .pointer("/counts/total")
                .and_then(Value::as_u64)
                .is_some_and(|total| total >= 1),
            || format!("partial auto digest should retain successful diagnostics: {digest}"),
        )?;
        require(
            json_contains_str(&digest, "partial auto diagnostic"),
            || format!("partial auto digest should include printf diagnostic text: {digest}"),
        )?;

        let good = profile_status_named(&digest, "good_auto")?;
        require(
            good.get("state").and_then(Value::as_str) == Some("ran"),
            || format!("good configured auto profile should report ran: {digest}"),
        )?;
        require(
            good.get("diagnostic_count")
                .and_then(Value::as_u64)
                .is_some_and(|count| count >= 1),
            || format!("good configured auto profile should report diagnostics: {good}"),
        )?;
        let bad = profile_status_named(&digest, "bad_auto")?;
        require(
            bad.get("state").and_then(Value::as_str) == Some("failed"),
            || format!("bad configured auto profile should report failed: {digest}"),
        )?;
        require(
            bad.get("error_kind").and_then(Value::as_str).is_some(),
            || format!("failed configured auto profile should report error_kind: {bad}"),
        )?;
        Ok(())
    }

    /// Verifies that built-in Agda batch mode treats parseable nonzero exits as
    /// successful diagnostic digests.
    #[test]
    fn agda_batch_type_error_emits_json_digest_when_agda_exists() -> Result<(), Box<dyn Error>>
    {
        match Command::new("agda").arg("--version").output() {
            | Ok(_) => {},
            | Err(error) => {
                eprintln!("SKIP: agda unavailable in PATH: {error}");
                return Ok(());
            },
        }

        let temp_dir = create_temp_dir("agda-batch")?;
        let bad_path = temp_dir.join("Bad.agda");
        fs::write(
            &bad_path,
            concat!("module Bad where\n\n", "bad : Set\n", "bad = Set\n"),
        )?;
        let output = run_aifix([
            OsString::from("batch"),
            OsString::from("agda"),
            OsString::from("--protocol"),
            OsString::from("agda-text"),
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("--max-diagnostics"),
            OsString::from("4"),
            OsString::from("--"),
            OsString::from("-i"),
            temp_dir.as_os_str().to_os_string(),
            bad_path.as_os_str().to_os_string(),
        ])?;
        let digest = successful_json(output)?;
        let expected_bad_path = bad_path.canonicalize()?;
        let expected_bad_path = path_to_str(&expected_bad_path)?;

        require(
            json_field_equals_str(&digest, "code", "UnequalSorts"),
            || format!("Agda batch digest should preserve the Agda code tag: {digest}"),
        )?;
        require(json_field_equals_str(&digest, "source", "agda"), || {
            format!("Agda batch digest should preserve the Agda source: {digest}")
        })?;
        require(
            json_field_equals_str(&digest, "file", expected_bad_path),
            || {
                format!(
                    "Agda batch digest should preserve the canonical temporary file path {expected_bad_path}: {digest}"
                )
            },
        )?;
        require(
            json_contains_str(&digest, "expression Set has type Set"),
            || format!("Agda batch digest should preserve the diagnostic message body: {digest}"),
        )?;
        Ok(())
    }

    /// Verifies that custom batch mode invokes a real local executable.
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
