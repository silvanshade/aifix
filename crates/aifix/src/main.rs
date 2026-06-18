//! Command-line entry point for the `aifix` diagnostic adapter.
//!
//! The binary is intentionally thin: clap owns argument decoding, while the
//! library owns protocol parsing, digest construction, batch execution, and
//! rendering. Keeping the boundary narrow makes the CLI easy for agents to
//! exercise and keeps behavior available to future non-CLI front ends.

use core::convert::Infallible;
use core::str::FromStr;
use std::ffi::OsString;
use std::io;
use std::io::Read as _;
use std::io::Write;
use std::process::ExitCode;

use aifix::adapter::parse_diagnostics;
use aifix::batch::run_configured_profile;
use aifix::batch::run_profile_with_limit;
use aifix::config::Config;
use aifix::config::config_paths;
use aifix::digest::build_digest;
use aifix::error::AifixError;
use aifix::explain::Explain;
use aifix::explain::ExplainStatus;
use aifix::explain::explain_code;
use aifix::model::Invocation;
use aifix::model::OutputFormat;
use aifix::model::Protocol;
use aifix::render::render_digest;
use camino::Utf8Path;
use camino::Utf8PathBuf;
use clap::Args;
use clap::CommandFactory as _;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use clap_complete::Shell;
use clap_complete::generate;

/// Exit status used when the requested operation completed successfully.
///
/// # Contract
/// Preconditions: command execution returned `Ok(())`.
/// Postconditions: maps successful CLI completion to the platform success code.
/// Failure modes: none.
/// Panics: none.
const EXIT_SUCCESS: ExitCode = ExitCode::SUCCESS;

/// Exit status used when aifix itself failed to read, parse, run, or render.
///
/// # Contract
/// Preconditions: command execution returned a `CliError`.
/// Postconditions: maps CLI failure to the platform failure code.
/// Failure modes: none.
/// Panics: none.
const EXIT_FAILURE: ExitCode = ExitCode::FAILURE;

/// Parse arguments, run the requested command, and translate failures to
/// process exits.
///
/// # Contract
/// Preconditions: standard CLI argument decoding must be available to clap.
/// Postconditions: returns success only after the selected command completed.
/// Failure modes: writes a human-readable error to stderr and returns failure.
/// Panics: none.
fn main() -> ExitCode
{
    match run() {
        | Ok(()) => EXIT_SUCCESS,
        | Err(error) => {
            if writeln!(io::stderr().lock(), "aifix: {error}").is_err() {
                // Stderr is already unavailable; preserve the original failure
                // exit.
                return EXIT_FAILURE;
            }
            EXIT_FAILURE
        },
    }
}

/// Execute the selected subcommand.
///
/// # Contract
/// Preconditions: arguments must satisfy clap's generated parser invariants.
/// Postconditions: dispatches exactly one subcommand implementation.
/// Failure modes: propagates fallible subcommand errors; completion generation
/// reports no recoverable errors through clap-complete. The MCP server owns
/// its stdio loop until stdin closes. Panics: none.
fn run() -> Result<(), CliError>
{
    let cli = Cli::parse();

    match cli.command {
        | Command::Pipeline(command) => run_pipeline(command),
        | Command::Batch(command) => run_batch(command),
        | Command::Explain(command) => run_explain(command),
        | Command::Config(command) => run_config(&command),
        | Command::Mcp => aifix::mcp::run_stdio_server().map_err(CliError::from),
        | Command::Completions(command) => {
            run_completions(&command);
            Ok(())
        },
    }
}

/// Agent-first diagnostic adapter command line.
///
/// # Contract
/// Preconditions: clap owns construction of this value from process arguments.
/// Postconditions: contains one validated top-level subcommand.
/// Failure modes: clap reports invalid argument shapes before this value
/// exists. Panics: none.
#[derive(Debug, Parser)]
#[command(name = "aifix")]
#[command(
    version,
    about = "Convert tool diagnostics into LLM-friendly repair digests."
)]
struct Cli
{
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Command,
}

/// Supported top-level commands.
///
/// # Contract
/// Preconditions: variants must stay aligned with the documented CLI surface.
/// Postconditions: each variant maps to one dispatch arm in `run`.
/// Failure modes: invalid spellings are rejected by clap before dispatch.
/// Panics: none.
#[derive(Debug, Subcommand)]
enum Command
{
    /// Ingest diagnostics from stdin or a file and render a digest.
    Pipeline(PipelineCommand),
    /// Invoke a configured diagnostic tool profile and render a digest.
    Batch(BatchCommand),
    /// Explain diagnostic codes without making network requests.
    Explain(ExplainCommand),
    /// Inspect aifix configuration discovery details.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Run the Model Context Protocol server over newline-delimited stdio.
    Mcp,
    /// Generate a shell completion script on standard output.
    Completions(CompletionsCommand),
}

/// Arguments for pipeline mode.
///
/// # Contract
/// Preconditions: `input` is either `-` or an OS path accepted by the platform.
/// Postconditions: optional protocol, format, and limit override discovered
/// configuration. Failure modes: invalid enum spellings are rejected by clap.
/// Panics: none.
#[derive(Debug, Args)]
struct PipelineCommand
{
    /// Input diagnostic protocol to parse.
    #[arg(long, value_enum)]
    protocol: Option<CliProtocol>,

    /// Output representation for the digest.
    #[arg(long, value_enum)]
    format: Option<CliOutputFormat>,

    /// Input file path, or '-' to read standard input.
    #[arg(long, default_value = "-")]
    input: InputPath,

    /// Maximum number of sample diagnostics retained in the digest.
    #[arg(long)]
    max_diagnostics: Option<usize>,
}

/// Arguments for batch mode.
///
/// # Contract
/// Preconditions: `profile` names either a configured profile or the built-in
/// custom path. Postconditions: optional protocol, format, working directory,
/// and limit override configuration. Failure modes: missing profile
/// configuration or failing child commands are reported as errors.
/// Panics: none.
#[derive(Debug, Args)]
struct BatchCommand
{
    /// Profile name to execute: rust, typescript, nushell, or custom.
    profile: String,

    /// Protocol used to parse the invoked tool output.
    #[arg(long, value_enum)]
    protocol: Option<CliProtocol>,

    /// Output representation for the digest.
    #[arg(long, value_enum)]
    format: Option<CliOutputFormat>,

    /// Working directory for configuration discovery and command execution.
    #[arg(long)]
    cwd: Option<Utf8PathBuf>,

    /// Maximum number of sample diagnostics retained in the digest.
    #[arg(long)]
    max_diagnostics: Option<usize>,

    /// Extra profile arguments, or the full command argv for the custom
    /// profile.
    #[arg(last = true)]
    extra_args: Vec<OsString>,
}

/// Arguments for deterministic diagnostic-code explanations.
///
/// # Contract
/// Preconditions: `source` is a diagnostic producer name such as `rustc` or
/// `clippy`. Postconditions: each requested code produces one explanation
/// block. Failure modes: stdout write errors are propagated.
/// Panics: none.
#[derive(Debug, Args)]
struct ExplainCommand
{
    /// Diagnostic source, such as rustc, clippy, typescript, or oxlint.
    source: String,

    /// Optional code values to explain. When omitted, explains the source only.
    codes: Vec<String>,
}

/// Arguments for shell completion generation.
///
/// # Contract
/// Preconditions: `shell` must be one of clap-complete's supported shells.
/// Postconditions: writes a completion script to standard output with the
/// explicit `aifix` binary name. Failure modes: none are surfaced by
/// clap-complete's generation API.
/// Panics: none.
#[derive(Debug, Args)]
struct CompletionsCommand
{
    /// Shell syntax to generate.
    #[arg(value_enum)]
    shell: Shell,
}

/// Arguments for configuration inspection.
///
/// # Contract
/// Preconditions: variants must stay aligned with configuration inspection
/// dispatch. Postconditions: each variant maps to one inspection action.
/// Failure modes: discovery or stdout write errors are propagated.
/// Panics: none.
#[derive(Debug, Subcommand)]
enum ConfigCommand
{
    /// Print the user and project configuration paths considered by aifix.
    Paths,
}

/// CLI spellings for supported input protocols.
///
/// # Contract
/// Preconditions: variants must mirror the library protocol enum.
/// Postconditions: conversion into `Protocol` is total and allocation-free.
/// Failure modes: invalid spellings are rejected by clap.
/// Panics: none.
#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliProtocol
{
    /// Let the adapter infer the protocol from the input shape.
    Auto,
    /// Already-normalized aifix JSON diagnostics.
    AifixJson,
    /// Rust compiler-message JSON Lines emitted by rustc or clippy.
    ClippyJson,
    /// Plain TypeScript compiler output with pretty printing disabled.
    TypescriptText,
    /// Language Server Protocol diagnostic arrays or publishDiagnostics params.
    LspJson,
    /// Nushell linter text output, parsed as generic diagnostic lines.
    NushellText,
}

/// CLI spellings for digest render formats.
///
/// # Contract
/// Preconditions: variants must mirror the library output format enum.
/// Postconditions: conversion into `OutputFormat` is total and allocation-free.
/// Failure modes: invalid spellings are rejected by clap.
/// Panics: none.
#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum CliOutputFormat
{
    /// Full JSON digest with all retained fields.
    Json,
    /// JSON optimized for small agent context windows.
    CompactJson,
    /// Markdown guidance grouped by source and code.
    Markdown,
}

/// Input path wrapper that preserves `-` as standard input.
///
/// # Contract
/// Preconditions: raw values come from clap's path argument parser.
/// Postconditions: stores the value exactly as provided for later source
/// selection. Failure modes: parsing is infallible.
/// Panics: none.
#[derive(Clone, Debug)]
struct InputPath
{
    /// Raw command-line value.
    raw: String,
}

/// Binary-local error wrapper for IO plus library failures.
///
/// # Contract
/// Preconditions: errors originate from CLI IO or library operations.
/// Postconditions: preserves source errors for display and exit translation.
/// Failure modes: non-UTF-8 current directories and batch extra arguments are
/// represented explicitly.
/// Panics: none.
#[derive(Debug, thiserror::Error)]
enum CliError
{
    /// Reading or writing process streams failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The core aifix library rejected the request.
    #[error(transparent)]
    Aifix(#[from] AifixError),

    /// The process current directory is not valid UTF-8.
    #[error("current directory is not valid UTF-8: {0}")]
    NonUtf8CurrentDir(String),

    /// A batch extra argument was not valid UTF-8.
    #[error("batch extra argument {index} is not valid UTF-8")]
    NonUtf8ExtraArg
    {
        /// Zero-based position within the arguments after `--`.
        index: usize,
    },
}

impl FromStr for InputPath
{
    type Err = Infallible;

    /// Store the path exactly as clap received it.
    ///
    /// # Contract
    /// Preconditions: `raw` is the command-line token supplied for `--input`.
    /// Postconditions: returns an `InputPath` preserving the token
    /// byte-for-byte as UTF-8. Failure modes: none; the parser is
    /// infallible. Panics: none.
    fn from_str(raw: &str) -> Result<Self, Self::Err>
    {
        Ok(Self {
            raw: raw.to_owned(),
        })
    }
}

impl From<CliProtocol> for Protocol
{
    /// Convert clap's value enum into the library protocol enum.
    ///
    /// # Contract
    /// Preconditions: `value` is a valid CLI protocol variant.
    /// Postconditions: returns the semantically identical library protocol
    /// variant. Failure modes: none.
    /// Panics: none.
    #[inline]
    fn from(value: CliProtocol) -> Self
    {
        match value {
            | CliProtocol::Auto => Self::Auto,
            | CliProtocol::AifixJson => Self::AifixJson,
            | CliProtocol::ClippyJson => Self::ClippyJson,
            | CliProtocol::TypescriptText => Self::TypescriptText,
            | CliProtocol::LspJson => Self::LspJson,
            | CliProtocol::NushellText => Self::NushellText,
        }
    }
}

impl From<CliOutputFormat> for OutputFormat
{
    /// Convert clap's value enum into the library output-format enum.
    ///
    /// # Contract
    /// Preconditions: `value` is a valid CLI output-format variant.
    /// Postconditions: returns the semantically identical library output-format
    /// variant. Failure modes: none.
    /// Panics: none.
    #[inline]
    fn from(value: CliOutputFormat) -> Self
    {
        match value {
            | CliOutputFormat::Json => Self::Json,
            | CliOutputFormat::CompactJson => Self::CompactJson,
            | CliOutputFormat::Markdown => Self::Markdown,
        }
    }
}

/// Read diagnostics, build a digest, and write the selected representation.
///
/// # Contract
/// Preconditions: `command` came from clap and may override discovered
/// configuration. Postconditions: writes exactly one rendered digest to stdout
/// on success. Failure modes: current directory, configuration, input, parser,
/// digest, or stdout errors propagate. Panics: debug assertion failure only if
/// clap provides an empty non-stdin input path.
fn run_pipeline(command: PipelineCommand) -> Result<(), CliError>
{
    debug_assert!(
        command.input.raw == "-" || !command.input.raw.is_empty(),
        "clap should supply a stdin marker or non-empty input path"
    );
    let cwd = current_utf8_dir()?;
    let loaded_config = Config::discover(&cwd)?;
    let protocol = command
        .protocol
        .map(Protocol::from)
        .or(loaded_config.config.default_protocol)
        .unwrap_or(Protocol::Auto);
    let format = command
        .format
        .map(OutputFormat::from)
        .or(loaded_config.config.default_format)
        .unwrap_or(OutputFormat::Markdown);
    let max_diagnostics = command
        .max_diagnostics
        .or(loaded_config.config.max_diagnostics);
    let input = read_input(&command.input)?;
    let diagnostics = parse_diagnostics(protocol, &input)?;
    let invocation = Invocation::pipeline(protocol, command.input.raw);
    let digest = build_digest(diagnostics, invocation, max_diagnostics);

    write_digest(&digest, format)
}

/// Execute a configured profile, then write the digest returned by the library.
///
/// # Contract
/// Preconditions: `command.profile` names either a configured profile or a
/// custom invocation. Postconditions: writes exactly one rendered digest for
/// the executed command. Failure modes: current directory, configuration, child
/// execution, parser, digest, or stdout errors propagate. Panics: debug
/// assertion failure only if clap provides an empty profile name.
fn run_batch(command: BatchCommand) -> Result<(), CliError>
{
    debug_assert!(
        !command.profile.is_empty(),
        "clap should require a non-empty batch profile name"
    );
    let cwd = match command.cwd {
        | Some(path) => path,
        | None => current_utf8_dir()?,
    };
    let loaded_config = Config::discover(&cwd)?;
    let profile_config = loaded_config.config.profiles.get(&command.profile);
    let protocol = command
        .protocol
        .map(Protocol::from)
        .or_else(|| {
            let profile = profile_config?;
            profile.protocol
        })
        .or(loaded_config.config.default_protocol)
        .unwrap_or(Protocol::Auto);
    let format = command
        .format
        .map(OutputFormat::from)
        .or_else(|| {
            let profile = profile_config?;
            profile.format
        })
        .or(loaded_config.config.default_format)
        .unwrap_or(OutputFormat::Markdown);
    let profile_max_diagnostics = match profile_config {
        | Some(profile) => profile.max_diagnostics,
        | None => None,
    };
    let max_diagnostics = command
        .max_diagnostics
        .or(profile_max_diagnostics)
        .or(loaded_config.config.max_diagnostics);
    let extra_args = utf8_extra_args(command.extra_args)?;
    let digest = if let Some(profile) = profile_config {
        run_configured_profile(
            &command.profile,
            profile,
            &extra_args,
            protocol,
            &cwd,
            max_diagnostics,
        )?
    }
    else {
        run_profile_with_limit(
            &command.profile,
            &extra_args,
            protocol,
            &cwd,
            max_diagnostics,
        )?
    };

    write_digest(&digest, format)
}

/// Render one or more deterministic explanations.
///
/// # Contract
/// Preconditions: `command.source` is non-empty after clap parsing.
/// Postconditions: writes one explanation block for the source alone or for
/// each requested code. Failure modes: stdout write errors propagate.
/// Panics: debug assertion failure only if clap provides an empty explanation
/// source.
fn run_explain(command: ExplainCommand) -> Result<(), CliError>
{
    debug_assert!(
        !command.source.is_empty(),
        "clap should require a non-empty explanation source"
    );
    let mut stdout = io::stdout().lock();

    if command.codes.is_empty() {
        let explanation = explain_code(&command.source, None);
        write_explain(&mut stdout, &explanation)?;
        return Ok(());
    }

    for code in command.codes {
        let explanation = explain_code(&command.source, Some(&code));
        write_explain(&mut stdout, &explanation)?;
    }

    Ok(())
}

/// Run configuration inspection commands.
///
/// # Contract
/// Preconditions: `command` came from clap and identifies one inspection
/// action. Postconditions: writes the requested configuration detail to stdout.
/// Failure modes: configuration discovery or stdout write errors propagate.
/// Panics: none.
fn run_config(command: &ConfigCommand) -> Result<(), CliError>
{
    match command {
        | &ConfigCommand::Paths => write_config_paths(),
    }
}

/// Generate a completion script for the requested shell.
///
/// # Contract
/// Preconditions: clap can construct command metadata for this binary.
/// Postconditions: writes the selected shell completion script to stdout using
/// the explicit `aifix` binary name.
/// Failure modes: none are surfaced by clap-complete's generation API.
/// Panics: none.
fn run_completions(command: &CompletionsCommand)
{
    let mut clap_command = Cli::command();
    let mut stdout = io::stdout().lock();

    generate(command.shell, &mut clap_command, "aifix", &mut stdout);
}

/// Print the configuration files aifix considered for the current directory.
///
/// # Contract
/// Preconditions: the current directory must be valid UTF-8.
/// Postconditions: writes user and project configuration paths or `-` markers.
/// Failure modes: current directory, path discovery, or stdout write errors
/// propagate. Panics: none.
fn write_config_paths() -> Result<(), CliError>
{
    let cwd = current_utf8_dir()?;
    let paths = config_paths(&cwd)?;
    let mut stdout = io::stdout().lock();

    writeln!(
        stdout,
        "user: {}",
        display_optional_path(paths.user.as_deref())
    )?;
    writeln!(
        stdout,
        "project: {}",
        display_optional_path(paths.project.as_deref())
    )?;

    Ok(())
}

/// Write one explanation block to standard output.
///
/// # Contract
/// Preconditions: `stdout` is writable and `explanation` was built by the
/// library. Postconditions: writes reference, status, summary, and a trailing
/// blank line. Failure modes: stdout write errors propagate.
/// Panics: debug assertion failure only if library explanations omit their
/// reference.
fn write_explain(
    stdout: &mut impl Write,
    explanation: &Explain,
) -> Result<(), CliError>
{
    debug_assert!(
        !explanation.explain_ref.is_empty(),
        "library explanations should include a stable reference"
    );
    writeln!(stdout, "ref: {}", explanation.explain_ref)?;
    writeln!(
        stdout,
        "status: {}",
        explain_status_label(explanation.status)
    )?;
    writeln!(stdout, "summary: {}", explanation.summary)?;
    writeln!(stdout)?;

    Ok(())
}

/// Return the stable display spelling for an explanation confidence status.
///
/// # Contract
/// Preconditions: `status` was produced by the library classifier.
/// Postconditions: returns the same spelling the former Debug output used.
/// Failure modes: unknown future statuses are conservatively displayed as
/// `Unknown`. Panics: none.
fn explain_status_label(status: ExplainStatus) -> &'static str
{
    match status {
        | ExplainStatus::Known => "Known",
        | ExplainStatus::SourceKnown => "SourceKnown",
        | _ => "Unknown",
    }
}

/// Read the requested diagnostic input source into memory.
///
/// # Contract
/// Preconditions: `input.raw` is `-` for stdin or a path readable by this
/// process. Postconditions: returns the full input text without protocol
/// interpretation. Failure modes: stdin or filesystem read errors propagate.
/// Panics: debug assertion failure only if clap provides an empty non-stdin
/// input path.
fn read_input(input: &InputPath) -> Result<String, CliError>
{
    debug_assert!(
        input.raw == "-" || !input.raw.is_empty(),
        "input source should be stdin marker or non-empty path"
    );
    if input.raw == "-" {
        let mut buffer = String::new();
        io::stdin().lock().read_to_string(&mut buffer)?;
        return Ok(buffer);
    }

    Ok(std::fs::read_to_string(&input.raw)?)
}

/// Convert batch extra arguments into strict UTF-8 strings.
///
/// # Contract
/// Preconditions: `extra_args` came from clap after the batch `--` separator.
/// Postconditions: returns every argument unchanged as UTF-8 and preserves
/// order. Failure modes: returns a CLI error naming the first argument whose OS
/// bytes are not valid UTF-8. Panics: none.
fn utf8_extra_args(extra_args: Vec<OsString>) -> Result<Vec<String>, CliError>
{
    let mut utf8_args = Vec::with_capacity(extra_args.len());
    for (index, arg) in extra_args.into_iter().enumerate() {
        match arg.into_string() {
            | Ok(value) => utf8_args.push(value),
            | Err(_) => {
                return Err(CliError::NonUtf8ExtraArg { index });
            },
        }
    }

    Ok(utf8_args)
}

/// Write a rendered digest to standard output.
///
/// # Contract
/// Preconditions: `digest` is internally consistent and `format` is supported
/// by the renderer. Postconditions: writes the rendered digest and ensures it
/// ends with a newline. Failure modes: render or stdout write errors propagate.
/// Panics: debug assertion failure only if the renderer returns empty output.
fn write_digest(
    digest: &aifix::model::Digest,
    format: OutputFormat,
) -> Result<(), CliError>
{
    let rendered = render_digest(digest, format)?;
    debug_assert!(
        !rendered.is_empty(),
        "digest renderer should emit non-empty output"
    );
    let mut stdout = io::stdout().lock();
    stdout.write_all(rendered.as_bytes())?;
    if !rendered.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }

    Ok(())
}

/// Return the process current directory as a UTF-8 path.
///
/// # Contract
/// Preconditions: the process has an accessible current directory.
/// Postconditions: returns a camino UTF-8 path buffer.
/// Failure modes: current directory IO errors or non-UTF-8 paths propagate.
/// Panics: none.
fn current_utf8_dir() -> Result<Utf8PathBuf, CliError>
{
    let cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(cwd)
        .map_err(|path| CliError::NonUtf8CurrentDir(path.to_string_lossy().into_owned()))
}

/// Return a printable path, using `-` when that path was not discovered.
///
/// # Contract
/// Preconditions: `path`, when present, already satisfies camino UTF-8
/// invariants. Postconditions: returns the original path string or the `-`
/// marker without allocation. Failure modes: none.
/// Panics: none.
fn display_optional_path(path: Option<&Utf8Path>) -> &str
{
    path.map_or("-", Utf8Path::as_str)
}
