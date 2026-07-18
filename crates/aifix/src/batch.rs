//! Batch profile execution.
//!
//! Batch mode builds an argv vector for a known or configured profile, invokes
//! it directly with `std::process::Command`, captures bounded stdout and stderr
//! separately, and returns a digest whenever the captured output is parseable.

use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Cursor;
use std::io::Read;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStderr;
use std::process::ChildStdout;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::thread;
use std::thread::JoinHandle;

use camino::Utf8Path;

use crate::adapter::AutoReaderProtocol;
use crate::adapter::detect_auto_protocol_reader;
use crate::adapter::parse_complete_json_reader;
use crate::adapter::parse_diagnostics_reader;
use crate::config::Config;
use crate::config::ProfileConfig;
use crate::digest::build_digest;
use crate::error::AifixError;
use crate::model::Digest;
use crate::model::Invocation;
use crate::model::OutputFormat;
use crate::model::ProfileRunState;
use crate::model::ProfileRunStatus;
use crate::model::Protocol;

/// Built-in Rust profile executable and command family.
const RUST_COMMAND_FAMILY: &str = "cargo";
/// Built-in TypeScript profile executable and command family.
const TYPESCRIPT_COMMAND_FAMILY: &str = "tsc";
/// Built-in Agda profile executable and command family.
const AGDA_COMMAND_FAMILY: &str = "agda";
/// Built-in Nushell profile executable and command family.
const NUSHELL_COMMAND_FAMILY: &str = "nu-lint";

/// Built-in Rust profile command.
const RUST_COMMAND: &[&str] = &[
    RUST_COMMAND_FAMILY,
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
const TYPESCRIPT_COMMAND: &[&str] = &[TYPESCRIPT_COMMAND_FAMILY, "--noEmit", "--pretty", "false"];

/// Built-in Agda profile command.
const AGDA_COMMAND: &[&str] = &[AGDA_COMMAND_FAMILY, "--no-libraries"];

/// Built-in Nushell profile command.
const NUSHELL_COMMAND: &[&str] = &[NUSHELL_COMMAND_FAMILY];

/// Synthetic profile name that runs every detected defaultable profile.
pub const AUTO_PROFILE: &str = "auto";
/// Built-in profile that runs Cargo/Clippy JSON diagnostics.
const RUST_PROFILE: &str = "rust";
/// Built-in profile that runs TypeScript compiler diagnostics.
const TYPESCRIPT_PROFILE: &str = "typescript";
/// Built-in profile that runs Agda text diagnostics.
const AGDA_PROFILE: &str = "agda";
/// Built-in profile that runs Nushell diagnostics.
const NUSHELL_PROFILE: &str = "nushell";
/// Built-in profile that executes caller-provided argv directly.
const CUSTOM_PROFILE: &str = "custom";
/// Bounded number of directory entries inspected for recursive project-shape
/// detection.
const DETECTION_ENTRY_LIMIT: usize = 4096;

/// Discoverable batch profile metadata shared by CLI and MCP surfaces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct BatchProfileInfo
{
    /// Stable profile name accepted by batch mode.
    pub name: String,
    /// Alternate names accepted for the same profile.
    pub aliases: Vec<String>,
    /// Default protocol associated with this profile.
    pub protocol: Protocol,
    /// Human-readable command family, normally the executable name.
    pub command_family: String,
    /// Profile origin, either `builtin` or `config`.
    pub source: String,
    /// Whether this profile participates in `batch auto`.
    pub defaultable: bool,
    /// Whether the profile accepts extra argv after `--`.
    pub extra_args: bool,
    /// Whether the profile appears applicable to the supplied working
    /// directory.
    pub detected: bool,
    /// Human-readable detection or skip reason.
    pub detection_reason: String,
}

/// Return discoverable batch profiles for `cwd`.
///
/// # Contract
/// - requires: `cwd` names the UTF-8 directory used for best-effort project
///   shape detection.
/// - ensures: returns built-in profiles plus configured profile names in stable
///   order, with configured metadata overriding matching built-ins.
/// - fails: none; unreadable directories are treated as not detected.
/// - panics: none.
#[must_use]
#[inline]
pub fn profile_catalog(
    config: &Config,
    cwd: &Utf8Path,
) -> Vec<BatchProfileInfo>
{
    let mut profiles = built_in_profile_catalog(config, cwd);
    for (name, profile) in &config.profiles {
        if built_in_profile_names().contains(&name.as_str()) {
            continue;
        }
        profiles.push(configured_profile_info(name, profile, cwd));
    }
    profiles
}

/// Return stable profile names accepted by batch profile selection.
///
/// # Contract
/// - requires: `config` is a merged runtime configuration.
/// - ensures: returns built-in profile names plus configured profile names in
///   sorted order without aliases.
/// - fails: none.
/// - panics: none.
#[must_use]
#[inline]
pub fn available_profile_names(config: &Config) -> Vec<String>
{
    let mut names = built_in_profile_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    for name in config.profiles.keys() {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.clone());
        }
    }
    names.sort();
    names
}

/// Report whether a profile name or built-in alias is known.
///
/// # Contract
/// - requires: `name` is user-provided profile text.
/// - ensures: recognizes built-ins, built-in aliases, and configured profiles.
/// - fails: none.
/// - panics: none.
#[must_use]
#[inline]
pub fn is_known_profile(
    name: &str,
    config: &Config,
) -> bool
{
    matches!(
        name,
        AUTO_PROFILE
            | RUST_PROFILE
            | TYPESCRIPT_PROFILE
            | "ts"
            | AGDA_PROFILE
            | NUSHELL_PROFILE
            | "nu"
            | CUSTOM_PROFILE
    ) || config.profiles.contains_key(name)
}

/// Construct recovery text for an unknown batch profile.
///
/// # Contract
/// - requires: `name` is the rejected profile name and `config` is merged.
/// - ensures: returns a message listing available profiles and discovery
///   commands usable by CLI and MCP callers.
/// - fails: allocation may abort through the global allocator; no recoverable
///   error is returned.
/// - panics: none.
#[must_use]
#[inline]
pub fn unknown_profile_message(
    name: &str,
    config: &Config,
) -> String
{
    let available = available_profile_names(config).join(", ");
    format!(
        "unknown batch profile `{name}`; available profiles: {available}. Run `aifix config profiles --format json` or MCP `aifix_batch_profiles` for recovery metadata."
    )
}

/// Return the built-in default protocol for a profile name or alias.
///
/// # Contract
/// - requires: `name` is a profile name or alias.
/// - ensures: returns the protocol default for built-in profiles and `None` for
///   unknown configured-only names.
/// - fails: none.
/// - panics: none.
#[must_use]
#[inline]
pub fn default_protocol_for_profile(name: &str) -> Option<Protocol>
{
    match name {
        | AUTO_PROFILE | CUSTOM_PROFILE => Some(Protocol::Auto),
        | RUST_PROFILE => Some(Protocol::ClippyJson),
        | TYPESCRIPT_PROFILE | "ts" => Some(Protocol::TypescriptText),
        | AGDA_PROFILE => Some(Protocol::AgdaText),
        | NUSHELL_PROFILE | "nu" => Some(Protocol::NushellText),
        | _ => None,
    }
}

/// Render profile catalog metadata for CLI or MCP surfaces.
///
/// # Contract
/// - requires: `profiles` was produced by [`profile_catalog`] or equivalent
///   trusted metadata construction.
/// - ensures: JSON formats serialize the metadata; Markdown emits one bullet
///   per profile with detection and defaultability facts.
/// - fails: returns JSON serialization errors for JSON output formats.
/// - panics: none.
///
/// # Errors
/// Returns an error when JSON serialization fails.
#[inline]
pub fn render_profile_catalog(
    profiles: &[BatchProfileInfo],
    format: OutputFormat,
) -> Result<String, AifixError>
{
    match format {
        | OutputFormat::Json => serde_json::to_string_pretty(profiles).map_err(AifixError::Json),
        | OutputFormat::CompactJson => serde_json::to_string(profiles).map_err(AifixError::Json),
        | OutputFormat::Markdown => Ok(render_profile_catalog_markdown(profiles)),
    }
}

/// Run every detected defaultable profile and aggregate successful diagnostics.
///
/// # Contract
/// - requires: `config` is merged and `cwd` is the direct execution directory.
/// - ensures: considers every defaultable profile, skips undetected profiles,
///   resolves each process budget from `max_output_override`, selected-profile
///   config, root config, then the default, continues after operational
///   failures, and returns a digest whose `profile_statuses` records every
///   considered outcome.
/// - fails: none for per-profile operational failures; those are recorded in
///   the returned digest status metadata.
/// - panics: none.
#[must_use]
#[inline]
pub fn run_auto_profile(
    config: &Config,
    cwd: &Utf8Path,
    max_diagnostics: Option<usize>,
    max_output_override: Option<usize>,
) -> Digest
{
    let catalog = profile_catalog(config, cwd);
    let mut statuses = Vec::new();
    let mut diagnostics = Vec::new();

    for profile in catalog.into_iter().filter(|profile| profile.defaultable) {
        if !profile.detected {
            statuses.push(ProfileRunStatus {
                profile: profile.name,
                state: ProfileRunState::Skipped,
                protocol: profile.protocol,
                command_family: profile.command_family,
                diagnostic_count: None,
                error_kind: None,
                error: None,
                reason: Some(profile.detection_reason),
            });
            continue;
        }

        let result = run_auto_selected_profile(config, &profile, cwd, max_output_override);
        match result {
            | Ok(digest) => {
                let diagnostic_count = digest.counts.total;
                diagnostics.extend(digest.diagnostics);
                statuses.push(ProfileRunStatus {
                    profile: profile.name,
                    state: ProfileRunState::Ran,
                    protocol: profile.protocol,
                    command_family: profile.command_family,
                    diagnostic_count: Some(diagnostic_count),
                    error_kind: None,
                    error: None,
                    reason: Some(profile.detection_reason),
                });
            },
            | Err(error) => {
                statuses.push(ProfileRunStatus {
                    profile: profile.name,
                    state: ProfileRunState::Failed,
                    protocol: profile.protocol,
                    command_family: profile.command_family,
                    diagnostic_count: None,
                    error_kind: Some(classify_error(&error).to_owned()),
                    error: Some(error.to_string()),
                    reason: Some(profile.detection_reason),
                });
            },
        }
    }

    let invocation = Invocation::with_cwd_path(
        vec![
            "aifix".to_owned(),
            "batch".to_owned(),
            AUTO_PROFILE.to_owned(),
        ],
        cwd,
        String::new(),
        String::new(),
        None,
    );
    let mut digest = build_digest(diagnostics, invocation, max_diagnostics);
    digest.profile_statuses = statuses;
    digest
}
/// Maximum bytes retained in memory from each child-process output stream.
///
/// Complete streams above this threshold spill to a private temporary file and
/// remain available to the parser.
pub const BATCH_STREAM_RETENTION_LIMIT: usize = 1024 * 1024;
/// Default maximum bytes accepted from each child-process output stream.
///
/// Spilling keeps memory bounded, while this larger processing budget prevents
/// accidental or malicious tools from consuming unbounded temporary storage.
pub const DEFAULT_BATCH_STREAM_OUTPUT_LIMIT: usize = 1024 * 1024 * 1024;

/// Resource and rendering limits for one batch execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BatchLimits
{
    /// Maximum diagnostics retained per digest group.
    pub max_diagnostics: Option<usize>,
    /// Maximum bytes accepted from each child-process output stream.
    pub max_output_bytes: usize,
}

impl Default for BatchLimits
{
    /// Use uncapped digest samples and the bounded default stream budget.
    #[inline]
    fn default() -> Self
    {
        Self {
            max_diagnostics: None,
            max_output_bytes: DEFAULT_BATCH_STREAM_OUTPUT_LIMIT,
        }
    }
}

impl BatchLimits
{
    /// Construct explicit batch limits.
    ///
    /// # Contract
    /// - ensures: preserves both caller-provided limits exactly.
    /// - fails: none.
    /// - panics: none.
    #[must_use]
    #[inline]
    pub const fn new(
        max_diagnostics: Option<usize>,
        max_output_bytes: usize,
    ) -> Self
    {
        Self {
            max_diagnostics,
            max_output_bytes,
        }
    }
}

/// Run a built-in or custom profile and return an uncapped digest.
///
/// # Contract
/// - requires: `profile` names a built-in profile or `custom`; custom profiles
///   require `extra_args` to contain the executable first.
/// - ensures: executes the selected argv in `cwd`, retains at most
///   [`BATCH_STREAM_RETENTION_LIMIT`] bytes per stream in memory, spills larger
///   output within [`DEFAULT_BATCH_STREAM_OUTPUT_LIMIT`], and returns uncapped
///   group samples when parsing succeeds.
/// - fails: returns invalid argument, process, UTF-8, or parser errors.
/// - panics: debug builds may panic if batch argv construction violates
///   documented non-empty executable invariants.
///
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
    run_profile_with_limits(profile, extra_args, protocol, cwd, BatchLimits::default())
}

/// Run a built-in or custom profile with explicit resource limits.
///
/// # Contract
/// - requires: `profile` names a built-in profile or `custom`; custom profiles
///   require `extra_args` to contain the executable first.
/// - ensures: executes the selected argv in `cwd`, retains at most
///   [`BATCH_STREAM_RETENTION_LIMIT`] bytes per stream in memory, spills larger
///   streams for incremental parsing, and applies `limits` to output processing
///   and digest samples.
/// - fails: returns invalid argument, process, UTF-8, or parser errors.
/// - panics: debug builds may panic if batch argv construction violates
///   documented non-empty executable invariants.
///
/// # Errors
/// Returns an error when profile resolution fails, process execution or bounded
/// stream processing fails, captured streams are not UTF-8, or parsing fails.
#[inline]
pub fn run_profile_with_limits(
    profile: &str,
    extra_args: &[String],
    protocol: Protocol,
    cwd: &Utf8Path,
    limits: BatchLimits,
) -> Result<Digest, AifixError>
{
    let argv = profile_command(profile, extra_args)?;
    run_argv(argv, protocol, cwd, limits)
}

/// Run an explicitly configured profile and cap digest samples per group.
///
/// # Contract
/// - requires: `config.argv`, when non-empty, starts with an executable;
///   otherwise `name` resolves to a built-in profile.
/// - ensures: executes configured argv plus `extra_args` in `cwd`, retains at
///   most [`BATCH_STREAM_RETENTION_LIMIT`] bytes per stream in memory, and
///   applies `limits` to output processing and digest samples.
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
    limits: BatchLimits,
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
    run_argv(argv, protocol, cwd, limits)
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
            return Err(AifixError::invalid_argument(unknown_profile_message(
                other,
                &Config::default(),
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

/// Execute one argv vector and incrementally parse bounded captured output.
///
/// # Contract
/// - requires: `command` contains an executable at index zero and any following
///   arguments are already split into argv form.
/// - ensures: preserves bounded stdout and stderr prefixes separately in the
///   invocation, spills larger streams, parses complete output without loading
///   it into one string, and applies `limits` to output bytes and digest
///   samples.
/// - fails: returns invalid argument for an empty command, process errors for
///   spawn, pipe, wait, join, spill, output-limit, or unparsable non-zero
///   output, UTF-8 errors for non-UTF-8 streams, or parser errors for parse
///   failures.
/// - panics: debug builds may panic if the executable string is empty; command
///   splitting itself uses `split_first` and returns an error for empty
///   commands.
///
/// # Errors
/// Returns an error when `command` is empty, process execution or bounded
/// stream processing fails, streams are not UTF-8, or parsing fails.
fn run_argv(
    command: Vec<String>,
    protocol: Protocol,
    cwd: &Utf8Path,
    limits: BatchLimits,
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
    let capture = run_child_captured(executable, arguments, cwd, limits.max_output_bytes)?;
    let CapturedOutput {
        status,
        stdout,
        stderr,
    } = capture;
    stdout.validate_utf8("stdout", executable)?;
    stderr.validate_utf8("stderr", executable)?;
    let parse_result = parse_captured_output(protocol, &stdout, &stderr);
    let stdout_bytes = stdout.total_bytes();
    let stderr_bytes = stderr.total_bytes();
    let stdout_text = stdout.into_retained_string("stdout", executable)?;
    let stderr_text = stderr.into_retained_string("stderr", executable)?;
    let invocation = Invocation::with_captured_output(
        command,
        cwd,
        stdout_text,
        stderr_text,
        stdout_bytes,
        stderr_bytes,
        status.code(),
    );

    match parse_result {
        | Ok(diagnostics) => Ok(build_digest(
            diagnostics,
            invocation,
            limits.max_diagnostics,
        )),
        | Err(error) if !status.success() => Err(AifixError::process(format!(
            "command exited with status {status} and output was not parseable: {error}",
            status = status_label(status.code())
        ))),
        | Err(error) => Err(error),
    }
}

/// Build metadata for all built-in profiles, applying configured overrides.
fn built_in_profile_catalog(
    config: &Config,
    cwd: &Utf8Path,
) -> Vec<BatchProfileInfo>
{
    built_in_profile_names()
        .iter()
        .map(|name| {
            config.profiles.get(*name).map_or_else(
                || built_in_profile_info(name, cwd),
                |profile| configured_profile_info(name, profile, cwd),
            )
        })
        .collect()
}

/// Return canonical built-in profile names.
fn built_in_profile_names() -> [&'static str; 6]
{
    [
        AUTO_PROFILE,
        RUST_PROFILE,
        TYPESCRIPT_PROFILE,
        AGDA_PROFILE,
        NUSHELL_PROFILE,
        CUSTOM_PROFILE,
    ]
}

/// Build metadata for one built-in profile.
fn built_in_profile_info(
    name: &str,
    cwd: &Utf8Path,
) -> BatchProfileInfo
{
    let (detected, detection_reason) = detect_builtin_profile(name, cwd);
    BatchProfileInfo {
        name: name.to_owned(),
        aliases: aliases_for_profile(name),
        protocol: default_protocol_for_profile(name).unwrap_or(Protocol::Auto),
        command_family: command_family_for_profile(name),
        source: "builtin".to_owned(),
        defaultable: matches!(
            name,
            RUST_PROFILE | TYPESCRIPT_PROFILE | AGDA_PROFILE | NUSHELL_PROFILE
        ),
        extra_args: !matches!(name, AUTO_PROFILE),
        detected,
        detection_reason,
    }
}

/// Build metadata for one configured profile.
fn configured_profile_info(
    name: &str,
    profile: &ProfileConfig,
    cwd: &Utf8Path,
) -> BatchProfileInfo
{
    let (builtin_detected, builtin_reason) = detect_builtin_profile(name, cwd);
    let detected = profile.auto || builtin_detected;
    let detection_reason = if profile.auto {
        "configured profile has auto = true".to_owned()
    }
    else {
        builtin_reason
    };
    BatchProfileInfo {
        name: name.to_owned(),
        aliases: aliases_for_profile(name),
        protocol: profile
            .protocol
            .or_else(|| default_protocol_for_profile(name))
            .unwrap_or(Protocol::Auto),
        command_family: profile
            .argv
            .first()
            .cloned()
            .unwrap_or_else(|| command_family_for_profile(name)),
        source: "config".to_owned(),
        defaultable: profile.auto
            || matches!(
                name,
                RUST_PROFILE | TYPESCRIPT_PROFILE | AGDA_PROFILE | NUSHELL_PROFILE
            ),
        extra_args: true,
        detected,
        detection_reason,
    }
}

/// Run one auto-selected profile without applying final aggregate sample caps.
fn run_auto_selected_profile(
    config: &Config,
    profile: &BatchProfileInfo,
    cwd: &Utf8Path,
    max_output_override: Option<usize>,
) -> Result<Digest, AifixError>
{
    let configured = config.profiles.get(&profile.name);
    let max_output_bytes = max_output_override
        .or_else(|| {
            let selected_config = configured?;
            selected_config.max_output_bytes
        })
        .or(config.max_output_bytes)
        .unwrap_or(DEFAULT_BATCH_STREAM_OUTPUT_LIMIT);
    let limits = BatchLimits::new(None, max_output_bytes);
    configured.map_or_else(
        || run_profile_with_limits(&profile.name, &[], profile.protocol, cwd, limits),
        |configured| {
            run_configured_profile(
                &profile.name,
                configured,
                &[],
                profile.protocol,
                cwd,
                limits,
            )
        },
    )
}

/// Render profile metadata as Markdown.
fn render_profile_catalog_markdown(profiles: &[BatchProfileInfo]) -> String
{
    let mut markdown = String::from("# aifix batch profiles\n\n");
    for profile in profiles {
        markdown.push_str("- `");
        markdown.push_str(&profile.name);
        markdown.push('`');
        if !profile.aliases.is_empty() {
            markdown.push_str(" (aliases: ");
            markdown.push_str(&profile.aliases.join(", "));
            markdown.push(')');
        }
        markdown.push_str(": protocol `");
        markdown.push_str(profile.protocol.as_str());
        markdown.push_str("`, command family `");
        markdown.push_str(&profile.command_family);
        markdown.push_str("`, source `");
        markdown.push_str(&profile.source);
        markdown.push_str("`, defaultable ");
        markdown.push_str(if profile.defaultable { "yes" } else { "no" });
        markdown.push_str(", detected ");
        markdown.push_str(if profile.detected { "yes" } else { "no" });
        markdown.push_str(" — ");
        markdown.push_str(&profile.detection_reason);
        markdown.push('\n');
    }
    markdown
}

/// Return aliases accepted for a built-in profile.
fn aliases_for_profile(name: &str) -> Vec<String>
{
    match name {
        | TYPESCRIPT_PROFILE => vec!["ts".to_owned()],
        | NUSHELL_PROFILE => vec!["nu".to_owned()],
        | _ => Vec::new(),
    }
}

/// Return the command family label for a profile.
fn command_family_for_profile(name: &str) -> String
{
    match name {
        | RUST_PROFILE => RUST_COMMAND_FAMILY,
        | TYPESCRIPT_PROFILE | "ts" => TYPESCRIPT_COMMAND_FAMILY,
        | AGDA_PROFILE => AGDA_COMMAND_FAMILY,
        | NUSHELL_PROFILE | "nu" => NUSHELL_COMMAND_FAMILY,
        | AUTO_PROFILE => AUTO_PROFILE,
        | CUSTOM_PROFILE => CUSTOM_PROFILE,
        | other => other,
    }
    .to_owned()
}

/// Best-effort built-in project shape detection.
fn detect_builtin_profile(
    name: &str,
    cwd: &Utf8Path,
) -> (bool, String)
{
    match name {
        | AUTO_PROFILE => (true, "auto profile is always available".to_owned()),
        | RUST_PROFILE => detect_file(cwd, "Cargo.toml", "Cargo.toml found"),
        | TYPESCRIPT_PROFILE | "ts" => detect_any_file(
            cwd,
            &["tsconfig.json", "jsconfig.json", "package.json"],
            "TypeScript or JavaScript project marker found",
        ),
        | AGDA_PROFILE => detect_recursive_extensions(
            cwd,
            &["agda", "lagda", "lagda.md", "agda-lib"],
            "Agda source or library file found",
        ),
        | NUSHELL_PROFILE | "nu" => detect_recursive_extensions(cwd, &["nu"], "Nushell file found"),
        | CUSTOM_PROFILE => (
            false,
            "custom requires explicit configured auto profile".to_owned(),
        ),
        | _ => (false, "configured profile is not auto-enabled".to_owned()),
    }
}

/// Detect one file directly below `cwd`.
fn detect_file(
    cwd: &Utf8Path,
    filename: &str,
    found_reason: &str,
) -> (bool, String)
{
    if cwd.join(filename).is_file() {
        (true, found_reason.to_owned())
    }
    else {
        (false, format!("{filename} not found"))
    }
}

/// Detect any direct child file below `cwd`.
fn detect_any_file(
    cwd: &Utf8Path,
    filenames: &[&str],
    found_reason: &str,
) -> (bool, String)
{
    if filenames
        .iter()
        .any(|filename| cwd.join(filename).is_file())
    {
        (true, found_reason.to_owned())
    }
    else {
        (false, format!("none of {} found", filenames.join(", ")))
    }
}

/// Detect files by extension using a bounded recursive directory walk.
fn detect_recursive_extensions(
    cwd: &Utf8Path,
    extensions: &[&str],
    found_reason: &str,
) -> (bool, String)
{
    let mut stack = vec![cwd.to_owned()];
    let mut entries_seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir)
        else {
            continue;
        };
        for entry in entries.flatten() {
            entries_seen += 1;
            if entries_seen > DETECTION_ENTRY_LIMIT {
                return (
                    false,
                    format!("no matching files found before {DETECTION_ENTRY_LIMIT}-entry limit"),
                );
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str())
            else {
                continue;
            };
            if should_skip_detection_dir(name) {
                continue;
            }
            let Ok(file_type) = entry.file_type()
            else {
                continue;
            };
            if file_type.is_dir() {
                if let Ok(path) = camino::Utf8PathBuf::from_path_buf(path) {
                    stack.push(path);
                }
            }
            else if extensions.iter().any(|extension| {
                name == format!(".{extension}") || name.ends_with(&format!(".{extension}"))
            }) {
                return (true, found_reason.to_owned());
            }
        }
    }
    (false, "no matching files found".to_owned())
}

/// Return whether recursive detection should skip a directory name.
fn should_skip_detection_dir(name: &str) -> bool
{
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".beads" | ".aifix"
    )
}

/// Classify an operational profile error for structured auto status metadata.
fn classify_error(error: &AifixError) -> &'static str
{
    match *error {
        | AifixError::InvalidArgument(ref message) if message.contains("unknown batch profile") => {
            "invalid-profile"
        },
        | AifixError::InvalidArgument(_) => "invalid-profile",
        | AifixError::Process(ref message)
            if message.contains("failed to run") && message.contains("No such file") =>
        {
            "tool-unavailable"
        },
        | AifixError::Process(ref message) if message.contains("output was not parseable") => {
            "parse-failure"
        },
        | AifixError::Process(_) | AifixError::Io { .. } => "command-invocation",
        | AifixError::Utf8(_) => "utf8",
        | AifixError::Config(_) | AifixError::TomlDeserialize(_) => "config",
        | AifixError::Parser(_) | AifixError::Json(_) => "parse-failure",
    }
}

/// Unique suffix source for private spill files.
static NEXT_SPILL_ID: AtomicU64 = AtomicU64::new(0);

/// Captured child-process result with stdout and stderr retained separately.
struct CapturedOutput
{
    /// Process exit status reported after the child completed.
    status: ExitStatus,
    /// Complete stdout storage plus its bounded retained prefix.
    stdout: CapturedStream,
    /// Complete stderr storage plus its bounded retained prefix.
    stderr: CapturedStream,
}

/// One child stream with bounded invocation retention and optional complete
/// spill storage.
struct CapturedStream
{
    /// Bytes retained for invocation metadata and complete small output.
    retained: Vec<u8>,
    /// Total bytes observed for the stream.
    total_bytes: usize,
    /// Last byte observed, used to preserve stream concatenation semantics.
    last_byte: Option<u8>,
    /// Private complete-output storage when the stream exceeded retention.
    spill: Option<SpillFile>,
}

impl CapturedStream
{
    /// Return the complete byte count observed for this stream.
    #[must_use]
    #[inline]
    fn total_bytes(&self) -> usize
    {
        self.total_bytes
    }

    /// Return whether this stream produced no bytes.
    #[must_use]
    #[inline]
    fn is_empty(&self) -> bool
    {
        self.total_bytes == 0
    }

    /// Return the final byte observed for this stream.
    #[must_use]
    #[inline]
    fn last_byte(&self) -> Option<u8>
    {
        self.last_byte
    }

    /// Open the complete stream for parsing or validation.
    ///
    /// # Errors
    /// Returns a process error when a spilled stream cannot be reopened.
    fn open_reader(&self) -> Result<Box<dyn Read + '_>, AifixError>
    {
        self.spill.as_ref().map_or_else(
            || {
                let reader: Box<dyn Read + '_> = Box::new(Cursor::new(self.retained.as_slice()));
                Ok(reader)
            },
            SpillFile::open_reader,
        )
    }

    /// Validate complete stream bytes before parser dispatch.
    ///
    /// # Errors
    /// Returns process errors for spill reads and UTF-8 errors for invalid
    /// stream bytes.
    fn validate_utf8(
        &self,
        stream: &str,
        executable: &str,
    ) -> Result<(), AifixError>
    {
        let mut reader = self.open_reader()?;
        validate_reader_utf8(&mut *reader, stream, executable)
    }

    /// Convert the retained prefix into invocation metadata.
    ///
    /// # Errors
    /// Returns a UTF-8 error if the retained bytes are invalid. A valid
    /// complete stream may end its retained prefix inside one scalar; that
    /// incomplete suffix is omitted.
    fn into_retained_string(
        self,
        stream: &str,
        executable: &str,
    ) -> Result<String, AifixError>
    {
        retained_utf8_string(self.retained, stream, executable)
    }
}

/// Private temporary file used for output above the retention threshold.
struct SpillFile
{
    /// File path removed when the capture leaves scope.
    path: PathBuf,
}

impl SpillFile
{
    /// Create one collision-resistant private spill file.
    ///
    /// # Errors
    /// Returns a process error when no file can be created.
    fn create(stream: &str) -> Result<(Self, File), AifixError>
    {
        for _ in 0_u8 .. 128_u8 {
            let sequence = NEXT_SPILL_ID.fetch_add(1, Ordering::Relaxed);
            let filename = format!("aifix-{}-{sequence}-{stream}.capture", std::process::id());
            let path = std::env::temp_dir().join(filename);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;

                options.mode(0o600)
            };
            match options.open(&path) {
                | Ok(file) => return Ok((Self { path }, file)),
                | Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {},
                | Err(source) => {
                    return Err(AifixError::process(format!(
                        "failed to create private {stream} spill file: {source}"
                    )));
                },
            }
        }

        Err(AifixError::process(format!(
            "failed to create private {stream} spill file after repeated name collisions"
        )))
    }

    /// Reopen complete spilled output for reading.
    ///
    /// # Errors
    /// Returns a process error when the file cannot be opened.
    fn open_reader(&self) -> Result<Box<dyn Read + '_>, AifixError>
    {
        let file = File::open(&self.path).map_err(|source| {
            AifixError::process(format!("failed to reopen spilled batch output: {source}"))
        })?;
        let reader: Box<dyn Read> = Box::new(file);
        Ok(reader)
    }
}

impl Drop for SpillFile
{
    /// Remove private output storage after parsing and invocation construction.
    fn drop(&mut self)
    {
        drop(fs::remove_file(&self.path));
    }
}

/// Spawn a command and process stdout and stderr with explicit per-stream
/// budgets.
///
/// # Contract
/// - requires: `executable` is a non-empty command name and `arguments` are
///   already split into argv entries.
/// - ensures: waits for the process, preserves separated bounded prefixes,
///   spills larger complete streams, and accepts no more than
///   `max_output_bytes` for either stream.
/// - fails: returns process errors for spawn, unavailable pipes, read, spill,
///   wait, reader-thread, or output-budget failures.
/// - panics: none; reader thread panics are converted into process errors.
///
/// # Errors
/// Returns an error when the child cannot be spawned, its pipes cannot be
/// captured, the child cannot be waited on, a stream reader fails, a stream
/// exceeds the budget, or a reader thread panics.
fn run_child_captured(
    executable: &str,
    arguments: &[String],
    cwd: &Utf8Path,
    max_output_bytes: usize,
) -> Result<CapturedOutput, AifixError>
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
    let stdout_reader = match spawn_stdout_reader(executable, stdout_pipe, max_output_bytes) {
        | Ok(reader) => reader,
        | Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        },
    };
    let stderr_reader = match spawn_stderr_reader(executable, stderr_pipe, max_output_bytes) {
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

    Ok(CapturedOutput {
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
    max_output_bytes: usize,
) -> Result<JoinHandle<Result<CapturedStream, AifixError>>, AifixError>
{
    spawn_stream_reader("stdout", executable, reader, max_output_bytes)
}

/// Spawn a bounded stderr reader thread.
fn spawn_stderr_reader(
    executable: &str,
    reader: ChildStderr,
    max_output_bytes: usize,
) -> Result<JoinHandle<Result<CapturedStream, AifixError>>, AifixError>
{
    spawn_stream_reader("stderr", executable, reader, max_output_bytes)
}

/// Spawn a reader thread that spills after the in-memory retention limit.
///
/// # Contract
/// - requires: `stream` and `executable` are diagnostic labels, and `reader`
///   yields bytes from one child-process pipe.
/// - ensures: returns a join handle whose successful value retains at most
///   [`BATCH_STREAM_RETENTION_LIMIT`] bytes in memory and stores no more than
///   `max_output_bytes` total bytes.
/// - fails: thread-spawn errors are returned immediately; read, spill, and
///   output-limit failures are returned by the thread.
/// - panics: none.
fn spawn_stream_reader<Reader>(
    stream: &'static str,
    executable: &str,
    mut reader: Reader,
    max_output_bytes: usize,
) -> Result<JoinHandle<Result<CapturedStream, AifixError>>, AifixError>
where
    Reader: Read + Send + 'static,
{
    let executable = executable.to_owned();
    thread::Builder::new()
        .spawn(move || read_stream_captured(stream, &executable, &mut reader, max_output_bytes))
        .map_err(|source| AifixError::process(format!("failed to spawn {stream} reader: {source}")))
}

/// Read one stream with bounded memory and bounded total storage.
///
/// # Contract
/// - requires: `reader` is an open process stream, `stream` names it, and
///   `executable` names the producing command.
/// - ensures: retains at most [`BATCH_STREAM_RETENTION_LIMIT`] bytes in memory,
///   spills complete larger output privately, and accepts no more than
///   `max_output_bytes`.
/// - fails: returns process errors for read, spill, or output-limit failures.
/// - panics: none.
///
/// # Errors
/// Returns an error when reading or spilling fails or the stream exceeds its
/// processing budget.
fn read_stream_captured<Reader>(
    stream: &str,
    executable: &str,
    reader: &mut Reader,
    max_output_bytes: usize,
) -> Result<CapturedStream, AifixError>
where
    Reader: Read,
{
    let retention_limit = BATCH_STREAM_RETENTION_LIMIT.min(max_output_bytes);
    let mut retained = Vec::new();
    let mut spill: Option<(SpillFile, File)> = None;
    let mut total_bytes = 0usize;
    let mut last_byte = None;
    let mut buffer = [0_u8; 8192];

    loop {
        let read = reader.read(&mut buffer).map_err(|source| {
            AifixError::process(format!(
                "failed to read {stream} from `{executable}`: {source}"
            ))
        })?;
        if read == 0 {
            if let Some((spill_file, mut writer)) = spill {
                writer.flush().map_err(|source| {
                    AifixError::process(format!(
                        "failed to flush spilled {stream} from `{executable}`: {source}"
                    ))
                })?;
                drop(writer);
                return Ok(CapturedStream {
                    retained,
                    total_bytes,
                    last_byte,
                    spill: Some(spill_file),
                });
            }
            return Ok(CapturedStream {
                total_bytes: retained.len(),
                last_byte: retained.last().copied(),
                retained,
                spill: None,
            });
        }

        let Some(chunk) = buffer.get(.. read)
        else {
            return Err(AifixError::process(format!(
                "{stream} from `{executable}` produced an invalid read length"
            )));
        };
        let Some(next_total) = total_bytes.checked_add(read)
        else {
            return Err(AifixError::output_limit(
                stream,
                executable,
                max_output_bytes,
            ));
        };
        if next_total > max_output_bytes {
            return Err(AifixError::output_limit(
                stream,
                executable,
                max_output_bytes,
            ));
        }

        let wrote_to_spill: bool = spill.as_mut().map_or_else(
            || Ok(false),
            |spilled| -> Result<bool, AifixError> {
                spilled.1.write_all(chunk).map_err(|source| {
                    AifixError::process(format!(
                        "failed to spill {stream} from `{executable}`: {source}"
                    ))
                })?;
                Ok(true)
            },
        )?;
        if !wrote_to_spill {
            if next_total <= retention_limit {
                retained.extend_from_slice(chunk);
            }
            else {
                let retained_from_chunk = retention_limit.saturating_sub(retained.len());
                let Some((retained_chunk, spilled_chunk)) =
                    chunk.split_at_checked(retained_from_chunk)
                else {
                    return Err(AifixError::process(format!(
                        "{stream} from `{executable}` produced an invalid retained prefix length"
                    )));
                };
                retained.extend_from_slice(retained_chunk);
                let (spill_file, mut writer) = SpillFile::create(stream)?;
                writer.write_all(&retained).map_err(|source| {
                    AifixError::process(format!(
                        "failed to initialize spilled {stream} from `{executable}`: {source}"
                    ))
                })?;
                writer.write_all(spilled_chunk).map_err(|source| {
                    AifixError::process(format!(
                        "failed to spill {stream} from `{executable}`: {source}"
                    ))
                })?;
                spill = Some((spill_file, writer));
            }
        }

        total_bytes = next_total;
        last_byte = chunk.last().copied();
    }
}

/// Join a stream-reader thread and surface both returned and panic failures.
///
/// # Contract
/// - requires: `handle` belongs to a reader spawned by [`spawn_stream_reader`],
///   `stream` names the captured stream, and `executable` names the child.
/// - ensures: returns the complete bounded stream representation.
/// - fails: propagates reader errors and maps thread panics to process errors.
/// - panics: none.
///
/// # Errors
/// Returns an error when the reader returned one or the thread panicked.
fn join_reader(
    handle: JoinHandle<Result<CapturedStream, AifixError>>,
    stream: &str,
    executable: &str,
) -> Result<CapturedStream, AifixError>
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

/// Parse complete stdout followed by stderr without joining them in memory.
///
/// # Errors
/// Returns process, IO, JSON, or parser errors from stream reopening and
/// adapter dispatch.
fn parse_captured_output(
    protocol: Protocol,
    stdout: &CapturedStream,
    stderr: &CapturedStream,
) -> Result<Vec<crate::model::Diagnostic>, AifixError>
{
    let selected = if protocol == Protocol::Auto {
        detect_auto_protocol_reader(combined_captured_reader(stdout, stderr)?)?
    }
    else {
        AutoReaderProtocol::Selected(protocol)
    };

    match selected {
        | AutoReaderProtocol::Selected(selected) => {
            parse_diagnostics_reader(selected, combined_captured_reader(stdout, stderr)?)
        },
        | AutoReaderProtocol::CompleteJson => {
            parse_complete_json_reader(combined_captured_reader(stdout, stderr)?)
        },
    }
}

/// Reopen stdout followed by stderr as one buffered parser stream.
///
/// # Errors
/// Returns a process error when either spilled stream cannot be reopened.
fn combined_captured_reader<'capture>(
    stdout: &'capture CapturedStream,
    stderr: &'capture CapturedStream,
) -> Result<impl BufRead + 'capture, AifixError>
{
    let stdout_reader = stdout.open_reader()?;
    let stderr_reader = stderr.open_reader()?;
    let separator: &'static [u8] =
        if !stdout.is_empty() && !stderr.is_empty() && stdout.last_byte() != Some(b'\n') {
            b"\n"
        }
        else {
            b""
        };
    let combined = stdout_reader
        .chain(Cursor::new(separator))
        .chain(stderr_reader);
    Ok(BufReader::new(combined))
}

/// Validate UTF-8 incrementally without retaining the complete stream.
///
/// # Errors
/// Returns a process error for read failures and a UTF-8 error for malformed or
/// incomplete byte sequences.
fn validate_reader_utf8<Reader>(
    reader: &mut Reader,
    stream: &str,
    executable: &str,
) -> Result<(), AifixError>
where
    Reader: Read + ?Sized,
{
    let mut buffer = [0_u8; 8195];
    let mut pending = 0usize;
    let mut validated_bytes = 0usize;
    loop {
        let Some(read_buffer) = buffer.get_mut(pending ..)
        else {
            return Err(AifixError::process(
                "UTF-8 validator pending-byte count exceeded its buffer",
            ));
        };
        let read = reader.read(read_buffer).map_err(|source| {
            AifixError::process(format!(
                "failed to validate {stream} from `{executable}`: {source}"
            ))
        })?;
        let total = pending + read;
        if total == 0 {
            return Ok(());
        }
        if read == 0 {
            return Err(AifixError::utf8(format!(
                "{stream} from `{executable}` ended with incomplete UTF-8 at byte {validated_bytes}"
            )));
        }

        let Some(candidate) = buffer.get(.. total)
        else {
            return Err(AifixError::process(
                "UTF-8 validator read length exceeded its buffer",
            ));
        };
        match core::str::from_utf8(candidate) {
            | Ok(_) => {
                validated_bytes += total;
                pending = 0;
            },
            | Err(error) if error.error_len().is_none() => {
                let valid_up_to = error.valid_up_to();
                validated_bytes += valid_up_to;
                pending = total - valid_up_to;
                buffer.copy_within(valid_up_to .. total, 0);
            },
            | Err(error) => {
                return Err(AifixError::utf8(format!(
                    "{stream} from `{executable}` was not UTF-8 at byte {}",
                    validated_bytes + error.valid_up_to()
                )));
            },
        }
    }
}

/// Convert a validated retained prefix to text, omitting an incomplete suffix.
///
/// # Errors
/// Returns a UTF-8 error when `bytes` contains malformed data rather than only
/// a valid prefix ending inside one scalar.
fn retained_utf8_string(
    bytes: Vec<u8>,
    stream: &str,
    executable: &str,
) -> Result<String, AifixError>
{
    match String::from_utf8(bytes) {
        | Ok(text) => Ok(text),
        | Err(source) if source.utf8_error().error_len().is_none() => {
            let valid_up_to = source.utf8_error().valid_up_to();
            let mut retained_bytes = source.into_bytes();
            retained_bytes.truncate(valid_up_to);
            String::from_utf8(retained_bytes).map_err(|error| {
                AifixError::utf8(format!(
                    "retained {stream} from `{executable}` was not UTF-8: {error}"
                ))
            })
        },
        | Err(source) => Err(AifixError::utf8(format!(
            "retained {stream} from `{executable}` was not UTF-8: {source}"
        ))),
    }
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

    /// Verify that output above the retention threshold spills while remaining
    /// completely readable.
    #[test]
    fn captured_stream_spills_without_truncating_parser_input() -> Result<(), AifixError>
    {
        let payload = vec![b'x'; BATCH_STREAM_RETENTION_LIMIT.saturating_add(8193)];
        let mut reader = io::Cursor::new(payload.clone());
        let captured = read_stream_captured("stdout", "fixture", &mut reader, payload.len())?;

        assert_eq!(captured.total_bytes(), payload.len());
        let spill_path = captured
            .spill
            .as_ref()
            .map(|spill| spill.path.clone())
            .ok_or_else(|| AifixError::process("large stream did not spill"))?;
        assert!(
            spill_path.try_exists().map_err(AifixError::io)?,
            "spill file should exist while captured output is alive"
        );
        let mut complete = Vec::new();
        captured
            .open_reader()?
            .read_to_end(&mut complete)
            .map_err(AifixError::io)?;
        assert_eq!(complete, payload);
        drop(captured);
        assert!(
            !spill_path.try_exists().map_err(AifixError::io)?,
            "spill file should be removed when captured output drops"
        );
        Ok(())
    }

    /// Verify incremental UTF-8 validation across chunk boundaries and strict
    /// rejection of malformed complete streams and retained prefixes.
    #[test]
    fn utf8_validation_handles_boundaries_and_invalid_bytes() -> Result<(), AifixError>
    {
        let mut split_scalar = vec![b'x'; 8194];
        split_scalar.extend_from_slice("é".as_bytes());
        let mut valid_reader = io::Cursor::new(split_scalar);
        validate_reader_utf8(&mut valid_reader, "stdout", "fixture")?;

        let mut invalid = vec![b'x'; 8194];
        invalid.push(0xff);
        let mut invalid_reader = io::Cursor::new(invalid);
        let invalid_error = match validate_reader_utf8(&mut invalid_reader, "stdout", "fixture") {
            | Ok(()) => {
                return Err(AifixError::process(
                    "invalid complete stream unexpectedly passed UTF-8 validation",
                ));
            },
            | Err(error) => error,
        };
        assert!(
            invalid_error.to_string().contains("was not UTF-8 at byte"),
            "invalid complete stream should identify its UTF-8 failure: {invalid_error}"
        );

        let retained = retained_utf8_string(vec![b'x', 0xc3], "stdout", "fixture")?;
        assert_eq!(retained, "x");
        let retained_error = match retained_utf8_string(vec![0xff], "stdout", "fixture") {
            | Ok(_) => {
                return Err(AifixError::process(
                    "invalid retained prefix unexpectedly passed UTF-8 conversion",
                ));
            },
            | Err(error) => error,
        };
        assert!(
            retained_error.to_string().contains("was not UTF-8"),
            "invalid retained prefix should report UTF-8 failure: {retained_error}"
        );
        Ok(())
    }

    /// Verify that the explicit processing budget still rejects oversized
    /// output.
    #[test]
    fn captured_stream_rejects_processing_limit_overflow() -> Result<(), AifixError>
    {
        let payload = vec![b'x'; 17];
        let mut reader = io::Cursor::new(payload);
        let error = match read_stream_captured("stdout", "fixture", &mut reader, 16) {
            | Ok(_) => {
                return Err(AifixError::process(
                    "captured stream unexpectedly exceeded its processing budget",
                ));
            },
            | Err(error) => error,
        };
        let message = error.to_string();
        if message.contains("exceeded capture limit of 16 bytes") {
            return Ok(());
        }

        Err(AifixError::process(format!(
            "captured stream returned unexpected error: {message}"
        )))
    }
}
