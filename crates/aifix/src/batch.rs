//! Batch profile execution.
//!
//! Batch mode builds an argv vector for a known or configured profile, invokes
//! it directly with `std::process::Command`, captures bounded stdout and stderr
//! separately, and returns a digest whenever the captured output is parseable.

use std::io::Read;
use std::process::Child;
use std::process::ChildStderr;
use std::process::ChildStdout;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::thread;
use std::thread::JoinHandle;

use camino::Utf8Path;

use crate::adapter::parse_diagnostics;
use crate::config::ProfileConfig;
use crate::digest::build_digest;
use crate::error::AifixError;
use crate::model::Digest;
use crate::model::Invocation;
use crate::model::Protocol;

/// Built-in Rust profile command.
const RUST_COMMAND: &[&str] = &[
    "cargo",
    "clippy",
    "--quiet",
    "--message-format=json",
    "--all-targets",
    "--all-features",
    "--",
    "--cap-lints",
    "warn",
];

/// Built-in TypeScript profile command.
const TYPESCRIPT_COMMAND: &[&str] = &["tsc", "--noEmit", "--pretty", "false"];

/// Built-in Agda profile command.
const AGDA_COMMAND: &[&str] = &["agda", "--no-libraries"];

/// Built-in Nushell profile command.
const NUSHELL_COMMAND: &[&str] = &["nu-lint"];

/// Maximum bytes retained from each child-process output stream.
pub const BATCH_STREAM_CAPTURE_LIMIT: usize = 1024 * 1024;

/// Run a built-in or custom profile and return an uncapped digest.
///
/// # Contract
/// - requires: `profile` names a built-in profile or `custom`; custom profiles
///   require `extra_args` to contain the executable first.
/// - ensures: executes the selected argv in `cwd`, retains at most
///   [`BATCH_STREAM_CAPTURE_LIMIT`] bytes per stream, and returns a digest with
///   uncapped group samples when output parses successfully.
/// - fails: returns invalid argument, process, UTF-8, or parser errors.
/// - panics: debug builds may panic if batch argv construction violates
///   documented non-empty executable invariants.
///
/// # Errors
/// # Errors
/// Returns an error when profile resolution fails, process execution or bounded
/// capture fails, captured streams are not UTF-8, or diagnostic parsing fails.
#[inline]
pub fn run_profile(
    profile: &str,
    extra_args: &[String],
    protocol: Protocol,
    cwd: &Utf8Path,
) -> Result<Digest, AifixError>
{
    run_profile_with_limit(profile, extra_args, protocol, cwd, None)
}

/// Run a built-in or custom profile and cap digest samples per group.
///
/// # Contract
/// - requires: `profile` names a built-in profile or `custom`; custom profiles
///   require `extra_args` to contain the executable first.
/// - ensures: executes the selected argv in `cwd`, retains at most
///   [`BATCH_STREAM_CAPTURE_LIMIT`] bytes per stream, and returns a digest
///   whose per-group samples respect `max_diagnostics`.
/// - fails: returns invalid argument, process, UTF-8, or parser errors.
/// - panics: debug builds may panic if batch argv construction violates
///   documented non-empty executable invariants.
///
/// # Errors
/// # Errors
/// Returns an error when profile resolution fails, process execution or bounded
/// capture fails, captured streams are not UTF-8, or diagnostic parsing fails.
#[inline]
pub fn run_profile_with_limit(
    profile: &str,
    extra_args: &[String],
    protocol: Protocol,
    cwd: &Utf8Path,
    max_diagnostics: Option<usize>,
) -> Result<Digest, AifixError>
{
    let argv = profile_command(profile, extra_args)?;
    run_argv(argv, protocol, cwd, max_diagnostics)
}

/// Run an explicitly configured profile and cap digest samples per group.
///
/// # Contract
/// - requires: `config.argv`, when non-empty, starts with an executable;
///   otherwise `name` resolves to a built-in profile.
/// - ensures: executes configured argv plus `extra_args` in `cwd`, retains at
///   most [`BATCH_STREAM_CAPTURE_LIMIT`] bytes per stream, and returns a digest
///   whose per-group samples respect `max_diagnostics`.
/// - fails: returns invalid argument, process, UTF-8, or parser errors.
/// - panics: debug builds may panic if resolved configured argv is unexpectedly
///   empty before execution.
///
/// # Errors
/// # Errors
/// Returns an error when profile resolution fails, process execution or bounded
/// capture fails, captured streams are not UTF-8, or diagnostic parsing fails.
#[inline]
pub fn run_configured_profile(
    name: &str,
    config: &ProfileConfig,
    extra_args: &[String],
    protocol: Protocol,
    cwd: &Utf8Path,
    max_diagnostics: Option<usize>,
) -> Result<Digest, AifixError>
{
    let mut argv = if config.argv.is_empty() {
        profile_command(name, &[])?
    }
    else {
        config.argv.clone()
    };
    argv.extend(extra_args.iter().cloned());
    debug_assert!(
        !argv.is_empty(),
        "configured profile argv must include an executable before execution"
    );
    run_argv(argv, protocol, cwd, max_diagnostics)
}

/// Return the command argv for a built-in profile.
///
/// # Contract
/// - requires: `profile` is a user-provided profile name; `custom` requires
///   `extra_args` to contain the executable first.
/// - ensures: returns a non-empty argv vector for every successful profile
///   resolution, with `extra_args` appended for built-ins.
/// - fails: returns an invalid argument error for unknown profiles or an empty
///   custom command.
/// - panics: debug builds may panic if a successful built-in profile resolves
///   to an empty argv vector.
///
/// # Errors
/// # Errors
/// Returns an error when `profile` is unknown or when `custom` receives no
/// executable in `extra_args`.
#[inline]
pub fn profile_command(
    profile: &str,
    extra_args: &[String],
) -> Result<Vec<String>, AifixError>
{
    let mut argv = match profile {
        | "rust" => strings(RUST_COMMAND),
        | "typescript" | "ts" => strings(TYPESCRIPT_COMMAND),
        | "agda" => strings(AGDA_COMMAND),
        | "nushell" | "nu" => strings(NUSHELL_COMMAND),
        | "custom" => {
            if extra_args.is_empty() {
                return Err(AifixError::invalid_argument(
                    "custom profile requires an explicit command after --",
                ));
            }
            return Ok(extra_args.to_vec());
        },
        | other => {
            return Err(AifixError::invalid_argument(format!(
                "unknown batch profile `{other}`; expected rust, typescript, agda, nushell, or custom"
            )));
        },
    };
    argv.extend(extra_args.iter().cloned());
    debug_assert!(
        !argv.is_empty(),
        "built-in profile argv must include an executable"
    );
    Ok(argv)
}

/// Execute one argv vector and parse bounded captured output.
///
/// # Contract
/// - requires: `command` must contain an executable at index zero and any
///   following arguments already split into argv form.
/// - ensures: preserves stdout and stderr separately in the invocation,
///   captures at most [`BATCH_STREAM_CAPTURE_LIMIT`] bytes per stream before
///   UTF-8 conversion, parses their combined text, and returns a digest capped
///   by `max_diagnostics`.
/// - fails: returns invalid argument for an empty command, process errors for
///   spawn, pipe, wait, join, capture-limit, or unparsable non-zero output,
///   UTF-8 errors for non-UTF-8 streams, or parser errors for parse failures.
/// - panics: debug builds may panic if the executable string is empty; command
///   splitting itself uses `split_first` and returns an error for empty
///   commands.
///
/// # Errors
/// # Errors
/// Returns an error when `command` is empty, process execution or bounded
/// capture fails, captured streams are not UTF-8, or diagnostic parsing fails.
fn run_argv(
    command: Vec<String>,
    protocol: Protocol,
    cwd: &Utf8Path,
    max_diagnostics: Option<usize>,
) -> Result<Digest, AifixError>
{
    let Some((executable, arguments)) = command.split_first()
    else {
        return Err(AifixError::invalid_argument(
            "batch command must include an executable",
        ));
    };
    debug_assert!(
        !executable.is_empty(),
        "batch executable should not be empty"
    );
    let capture = run_child_bounded(executable, arguments, cwd)?;

    let stdout = String::from_utf8(capture.stdout).map_err(|source| {
        AifixError::utf8(format!(
            "stdout from `{executable}` was not UTF-8: {source}"
        ))
    })?;
    let stderr = String::from_utf8(capture.stderr).map_err(|source| {
        AifixError::utf8(format!(
            "stderr from `{executable}` was not UTF-8: {source}"
        ))
    })?;
    let status_code = capture.status.code();
    let invocation = Invocation {
        command,
        cwd: Some(path_to_string(cwd)),
        stdout,
        stderr,
        exit_code: status_code,
    };

    let parse_input = parse_input(&invocation);
    match parse_diagnostics(protocol, &parse_input) {
        | Ok(diagnostics) => Ok(build_digest(diagnostics, invocation, max_diagnostics)),
        | Err(error) if !capture.status.success() => Err(AifixError::process(format!(
            "command exited with status {status} and output was not parseable: {error}",
            status = status_label(capture.status.code())
        ))),
        | Err(error) => Err(error),
    }
}

/// Captured child-process result with stdout and stderr retained separately.
struct BoundedOutput
{
    /// Process exit status reported after the child completed.
    status: ExitStatus,
    /// Captured stdout bytes, never longer than [`BATCH_STREAM_CAPTURE_LIMIT`].
    stdout: Vec<u8>,
    /// Captured stderr bytes, never longer than [`BATCH_STREAM_CAPTURE_LIMIT`].
    stderr: Vec<u8>,
}

/// Spawn a command and capture stdout and stderr with explicit per-stream caps.
///
/// # Contract
/// - requires: `executable` is a non-empty command name and `arguments` are
///   already split into argv entries.
/// - ensures: waits for the process, preserves separated stdout/stderr bytes,
///   and never retains more than [`BATCH_STREAM_CAPTURE_LIMIT`] bytes for
///   either stream.
/// - fails: returns process errors for spawn, unavailable pipes, read failures,
///   wait failures, reader-thread failures, or capture-limit overflow.
/// - panics: none; reader thread panics are converted into process errors.
///
/// # Errors
/// # Errors
/// Returns an error when the child cannot be spawned, its pipes cannot be
/// captured, the child cannot be waited on, a stream reader fails, a stream
/// exceeds the cap, or a reader thread panics.
fn run_child_bounded(
    executable: &str,
    arguments: &[String],
    cwd: &Utf8Path,
) -> Result<BoundedOutput, AifixError>
{
    let mut child = Command::new(executable)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AifixError::process(format!("failed to run `{executable}`: {source}")))?;

    let stdout_pipe = match take_stdout(&mut child, executable) {
        | Ok(stream) => stream,
        | Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        },
    };
    let stderr_pipe = match take_stderr(&mut child, executable) {
        | Ok(stream) => stream,
        | Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        },
    };
    let stdout_reader = match spawn_stdout_reader(executable, stdout_pipe) {
        | Ok(reader) => reader,
        | Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        },
    };
    let stderr_reader = match spawn_stderr_reader(executable, stderr_pipe) {
        | Ok(reader) => reader,
        | Err(error) => {
            terminate_child(&mut child);
            drop(join_reader(stdout_reader, "stdout", executable));
            return Err(error);
        },
    };
    let status = match child.wait() {
        | Ok(status) => status,
        | Err(source) => {
            terminate_child(&mut child);
            drop(join_reader(stdout_reader, "stdout", executable));
            drop(join_reader(stderr_reader, "stderr", executable));
            return Err(AifixError::process(format!(
                "failed to wait for `{executable}`: {source}"
            )));
        },
    };
    let stdout_result = join_reader(stdout_reader, "stdout", executable);
    let stderr_result = join_reader(stderr_reader, "stderr", executable);
    let captured_stdout = stdout_result?;
    let captured_stderr = stderr_result?;

    Ok(BoundedOutput {
        status,
        stdout: captured_stdout,
        stderr: captured_stderr,
    })
}

/// Best-effort child cleanup used before returning setup or wait failures.
fn terminate_child(child: &mut Child)
{
    drop(child.kill());
    drop(child.wait());
}

/// Take the child stdout pipe or convert the impossible absence into an error.
/// # Errors
/// Returns an error when stdout is unexpectedly unavailable.
fn take_stdout(
    child: &mut Child,
    executable: &str,
) -> Result<ChildStdout, AifixError>
{
    child.stdout.take().ok_or_else(|| {
        AifixError::process(format!("failed to capture stdout pipe from `{executable}`"))
    })
}

/// Take the child stderr pipe or convert the impossible absence into an error.
/// # Errors
/// Returns an error when stderr is unexpectedly unavailable.
fn take_stderr(
    child: &mut Child,
    executable: &str,
) -> Result<ChildStderr, AifixError>
{
    child.stderr.take().ok_or_else(|| {
        AifixError::process(format!("failed to capture stderr pipe from `{executable}`"))
    })
}

/// Spawn a bounded stdout reader thread.
fn spawn_stdout_reader(
    executable: &str,
    reader: ChildStdout,
) -> Result<JoinHandle<Result<Vec<u8>, AifixError>>, AifixError>
{
    spawn_stream_reader("stdout", executable, reader)
}

/// Spawn a bounded stderr reader thread.
fn spawn_stderr_reader(
    executable: &str,
    reader: ChildStderr,
) -> Result<JoinHandle<Result<Vec<u8>, AifixError>>, AifixError>
{
    spawn_stream_reader("stderr", executable, reader)
}

/// Spawn a reader thread that captures one stream up to the configured limit.
///
/// # Contract
/// - requires: `stream` and `executable` are diagnostic labels, and `reader`
///   yields bytes from one child-process pipe.
/// - ensures: returns a join handle whose successful value contains no more
///   than [`BATCH_STREAM_CAPTURE_LIMIT`] bytes.
/// - fails: thread-spawn errors are returned immediately; read failures and
///   capture-limit overflow are returned by the thread.
/// - panics: none.
fn spawn_stream_reader<R>(
    stream: &'static str,
    executable: &str,
    mut reader: R,
) -> Result<JoinHandle<Result<Vec<u8>, AifixError>>, AifixError>
where
    R: Read + Send + 'static,
{
    let executable = executable.to_owned();
    thread::Builder::new()
        .spawn(move || read_stream_bounded(stream, &executable, &mut reader))
        .map_err(|source| AifixError::process(format!("failed to spawn {stream} reader: {source}")))
}

/// Read one stream into memory with an explicit byte cap.
///
/// # Contract
/// - requires: `reader` is an open process stream, `stream` names it, and
///   `executable` names the producing command.
/// - ensures: returns captured bytes whose length is less than or equal to
///   [`BATCH_STREAM_CAPTURE_LIMIT`].
/// - fails: returns process errors for IO failures or cap overflow.
/// - panics: none.
///
/// # Errors
/// # Errors
/// Returns an error when reading fails or the stream exceeds the cap.
fn read_stream_bounded<R>(
    stream: &str,
    executable: &str,
    reader: &mut R,
) -> Result<Vec<u8>, AifixError>
where
    R: Read,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).map_err(|source| {
            AifixError::process(format!(
                "failed to read {stream} from `{executable}`: {source}"
            ))
        })?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = BATCH_STREAM_CAPTURE_LIMIT.saturating_sub(output.len());
        if read > remaining {
            return Err(AifixError::output_limit(
                stream,
                executable,
                BATCH_STREAM_CAPTURE_LIMIT,
            ));
        }
        let Some(chunk) = buffer.get(.. read)
        else {
            return Err(AifixError::process(format!(
                "{stream} from `{executable}` produced an invalid read length"
            )));
        };
        output.extend_from_slice(chunk);
    }
}

/// Join a stream-reader thread and surface both returned and panic failures.
///
/// # Contract
/// - requires: `handle` belongs to a reader spawned by [`spawn_stream_reader`],
///   `stream` names the captured stream, and `executable` names the child
///   process.
/// - ensures: returns bounded bytes produced by the reader thread.
/// - fails: propagates reader errors and maps thread panics to process errors.
/// - panics: none.
///
/// # Errors
/// # Errors
/// Returns an error when the reader returned one or the thread panicked.
fn join_reader(
    handle: JoinHandle<Result<Vec<u8>, AifixError>>,
    stream: &str,
    executable: &str,
) -> Result<Vec<u8>, AifixError>
{
    handle.join().unwrap_or_else(|_| {
        Err(AifixError::process(format!(
            "{stream} reader for `{executable}` panicked"
        )))
    })
}

/// Convert a string slice command constant into owned argv values.
fn strings(values: &[&str]) -> Vec<String>
{
    debug_assert!(
        !values.is_empty(),
        "built-in command templates must include an executable"
    );
    let argv = values
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    debug_assert_eq!(
        argv.len(),
        values.len(),
        "owned argv conversion must preserve length"
    );
    argv
}

/// Combine stdout and stderr only for parsing while preserving them separately.
fn parse_input(invocation: &Invocation) -> String
{
    match (invocation.stdout.is_empty(), invocation.stderr.is_empty()) {
        | (true, true) => String::new(),
        | (false, true) => invocation.stdout.clone(),
        | (true, false) => invocation.stderr.clone(),
        | (false, false) => {
            let mut combined =
                String::with_capacity(invocation.stdout.len() + invocation.stderr.len() + 1);
            combined.push_str(&invocation.stdout);
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&invocation.stderr);
            combined
        },
    }
}

/// Render a path as UTF-8 text for invocation metadata.
fn path_to_string(path: &Utf8Path) -> String
{
    path.to_owned().into_string()
}

/// Render an exit code for error messages.
fn status_label(code: Option<i32>) -> String
{
    code.map_or_else(
        || "terminated by signal".to_owned(),
        |value| value.to_string(),
    )
}

/// Unit coverage for bounded process-stream capture helpers.
#[cfg(test)]
mod tests
{
    use std::io;

    use super::*;

    /// Verify that the stream reader rejects input one byte above the cap.
    #[test]
    fn bounded_stream_rejects_one_byte_over_cap() -> Result<(), AifixError>
    {
        let payload = vec![b'x'; BATCH_STREAM_CAPTURE_LIMIT.saturating_add(1)];
        let mut reader = io::Cursor::new(payload);
        let error = match read_stream_bounded("stdout", "fixture", &mut reader) {
            | Ok(_) => {
                return Err(AifixError::process(
                    "bounded stream reader unexpectedly accepted over-limit input",
                ));
            },
            | Err(error) => error,
        };
        let message = error.to_string();
        if message.contains("exceeded capture limit") {
            return Ok(());
        }

        Err(AifixError::process(format!(
            "bounded stream reader returned unexpected error: {message}"
        )))
    }
}
