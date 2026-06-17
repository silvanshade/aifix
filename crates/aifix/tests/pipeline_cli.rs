//! End-to-end CLI coverage for diagnostic ingestion paths.

/// CLI integration tests for real binary and fixture-based ingestion scenarios.
#[cfg(test)]
mod tests
{
    use core::error::Error;
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use std::process::Output;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use serde_json::Value;

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
