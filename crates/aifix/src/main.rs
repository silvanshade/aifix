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
use aifix::batch::AUTO_PROFILE;
use aifix::batch::available_profile_names;
use aifix::batch::default_protocol_for_profile;
use aifix::batch::is_known_profile;
use aifix::batch::profile_catalog;
use aifix::batch::render_profile_catalog;
use aifix::batch::run_auto_profile;
use aifix::batch::run_configured_profile;
use aifix::batch::run_profile_with_limit;
use aifix::batch::unknown_profile_message;
use aifix::config::Config;
use aifix::config::config_paths;
use aifix::digest::build_digest;
use aifix::error::AifixError;
use aifix::explain::Explain;
use aifix::explain::ExplainStatus;
use aifix::explain::explain_code;
use aifix::model::Digest;
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
const EXIT_SUCCESS: ExitCode = ExitCode::SUCCESS;

/// Exit status used when aifix itself failed to read, parse, run, or render.
const EXIT_FAILURE: ExitCode = ExitCode::FAILURE;

/// Parse arguments, run the requested command, and translate failures to
/// process exits.
///
/// # Contract
/// - requires: standard CLI argument decoding must be available to clap.
/// - ensures: returns success only after the selected command completed.
/// - fails: writes a human-readable error to stderr and returns failure.
/// - panics: none.
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
/// - requires: arguments must satisfy clap's generated parser invariants.
/// - ensures: dispatches exactly one subcommand implementation.
/// - fails: propagates fallible subcommand errors; completion generation
///   reports no recoverable errors through clap-complete. The MCP server owns
///   its stdio loop until stdin closes.
/// - panics: none.
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
#[derive(Debug, Subcommand)]
enum Command
{
    /// Ingest diagnostics from stdin or a file and render a digest.
    Pipeline(PipelineCommand),
    /// Invoke a diagnostic tool profile, defaulting to discovered `auto`.
    Batch(BatchCommand),
    /// Explain diagnostic codes without making network requests.
    Explain(ExplainCommand),
    /// Inspect aifix configuration and discoverable batch profiles.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Run the Model Context Protocol server over newline-delimited stdio.
    Mcp,
    /// Generate a shell completion script on standard output.
    Completions(CompletionsCommand),
}

/// Arguments for pipeline mode.
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

    /// Diagnostic code allowed when `--fail-on-diagnostics` is active.
    #[arg(long = "expected-code", alias = "allow-code")]
    expected_codes: Vec<String>,

    /// Exit non-zero when diagnostics outside the expected-code list remain.
    #[arg(long)]
    fail_on_diagnostics: bool,
}

/// Arguments for batch mode.
#[derive(Debug, Args)]
struct BatchCommand
{
    /// Profile name to execute; omit to run discovered `auto`.
    #[arg(default_value = AUTO_PROFILE)]
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

    /// Diagnostic code allowed when `--fail-on-diagnostics` is active.
    #[arg(long = "expected-code", alias = "allow-code")]
    expected_codes: Vec<String>,

    /// Exit non-zero when diagnostics outside the expected-code list remain.
    #[arg(long)]
    fail_on_diagnostics: bool,

    /// Extra profile arguments for named profiles, or full command argv for
    /// `custom`; not accepted by `auto`.
    #[arg(last = true)]
    extra_args: Vec<OsString>,
}

/// Arguments for deterministic diagnostic-code explanations.
#[derive(Debug, Args)]
struct ExplainCommand
{
    /// Diagnostic source, such as rustc, clippy, typescript, or oxlint.
    source: String,

    /// Optional code values to explain. When omitted, explains the source only.
    codes: Vec<String>,
}

/// Arguments for shell completion generation.
#[derive(Debug, Args)]
struct CompletionsCommand
{
    /// Shell syntax to generate.
    #[arg(value_enum)]
    shell: Shell,
}

/// Arguments for configuration inspection.
#[derive(Debug, Subcommand)]
enum ConfigCommand
{
    /// Print the user and project configuration paths considered by aifix.
    Paths,
    /// List built-in and configured batch profiles discoverable from a cwd.
    Profiles(ConfigProfilesCommand),
}

/// Arguments for batch profile discovery.
#[derive(Debug, Args)]
struct ConfigProfilesCommand
{
    /// Working directory used for configuration discovery and project-shape
    /// detection.
    #[arg(long)]
    cwd: Option<Utf8PathBuf>,

    /// Output representation for profile metadata.
    #[arg(long, value_enum, default_value = "markdown")]
    format: CliOutputFormat,
}

/// CLI spellings for supported input protocols.
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
    /// Plain Agda compiler diagnostics.
    AgdaText,
    /// Language Server Protocol diagnostic arrays or publishDiagnostics params.
    LspJson,
    /// Nushell linter text output, parsed as generic diagnostic lines.
    NushellText,
}

/// CLI spellings for digest render formats.
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
#[derive(Clone, Debug)]
struct InputPath
{
    /// Raw command-line value.
    raw: String,
}

/// Binary-local error wrapper for IO plus library failures.
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

    /// Gate mode saw diagnostics outside the expected-code allow-list.
    #[error("unexpected diagnostics remained after expected-code filtering: {count}")]
    UnexpectedDiagnostics
    {
        /// Number of deduplicated unexpected diagnostics represented by groups.
        count: usize,
    },
}

impl FromStr for InputPath
{
    type Err = Infallible;

    /// Store the path exactly as clap received it.
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
    #[inline]
    fn from(value: CliProtocol) -> Self
    {
        match value {
            | CliProtocol::Auto => Self::Auto,
            | CliProtocol::AifixJson => Self::AifixJson,
            | CliProtocol::ClippyJson => Self::ClippyJson,
            | CliProtocol::TypescriptText => Self::TypescriptText,
            | CliProtocol::AgdaText => Self::AgdaText,
            | CliProtocol::LspJson => Self::LspJson,
            | CliProtocol::NushellText => Self::NushellText,
        }
    }
}

impl From<CliOutputFormat> for OutputFormat
{
    /// Convert clap's value enum into the library output-format enum.
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
/// - requires: `command` came from clap and may override discovered
///   configuration.
/// - ensures: writes exactly one rendered digest to stdout before enforcing any
///   requested diagnostic gate.
/// - fails: current directory, configuration, input, parser, digest, stdout, or
///   diagnostic-gate errors propagate.
/// - panics: debug assertion failure only if clap provides an empty non-stdin
///   input path.
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
    let input_path = command.input;
    let expected_codes = command.expected_codes;
    let fail_on_diagnostics = command.fail_on_diagnostics;
    let input = read_input(&input_path)?;
    let diagnostics = parse_diagnostics(protocol, &input)?;
    let invocation = Invocation::pipeline(protocol, input_path.raw);
    let digest = build_digest(diagnostics, invocation, max_diagnostics);

    write_digest(&digest, format)?;
    enforce_diagnostic_gate(&digest, &expected_codes, fail_on_diagnostics)
}

/// Execute a batch profile, then write the digest returned by the library.
///
/// # Contract
/// - requires: `command.profile` names `auto`, a configured profile, a known
///   built-in profile, or `custom` with a command argv after `--`.
/// - ensures: omitted profiles run `auto`; named profiles choose protocol from
///   CLI, profile config, built-in default, global config, then `auto`.
/// - fails: current directory, configuration, unknown profile, child execution,
///   parser, digest, stdout, or diagnostic-gate errors propagate.
/// - panics: debug assertion failure only if clap provides an empty profile
///   name.
fn run_batch(command: BatchCommand) -> Result<(), CliError>
{
    debug_assert!(
        !command.profile.is_empty(),
        "clap should provide the default auto batch profile name"
    );
    let cwd = match command.cwd {
        | Some(path) => path,
        | None => current_utf8_dir()?,
    };
    let loaded_config = Config::discover(&cwd)?;
    let config = &loaded_config.config;
    let profile_name = command.profile;
    let profile_config = config.profiles.get(&profile_name);
    let format = command
        .format
        .map(OutputFormat::from)
        .or_else(|| {
            let profile = profile_config?;
            profile.format
        })
        .or(config.default_format)
        .unwrap_or(OutputFormat::Markdown);
    let profile_max_diagnostics = match profile_config {
        | Some(profile) => profile.max_diagnostics,
        | None => None,
    };
    let max_diagnostics = command
        .max_diagnostics
        .or(profile_max_diagnostics)
        .or(config.max_diagnostics);
    let expected_codes = command.expected_codes;
    let fail_on_diagnostics = command.fail_on_diagnostics;
    let extra_args = utf8_extra_args(command.extra_args)?;

    let digest = if profile_name == AUTO_PROFILE {
        if !extra_args.is_empty() {
            return Err(AifixError::invalid_argument(auto_extra_args_message(config)).into());
        }
        run_auto_profile(config, &cwd, max_diagnostics)
    }
    else {
        if profile_config.is_none() && !is_known_profile(&profile_name, config) {
            return Err(AifixError::invalid_argument(unknown_profile_message(
                &profile_name,
                config,
            ))
            .into());
        }
        let protocol = command
            .protocol
            .map(Protocol::from)
            .or_else(|| {
                let profile = profile_config?;
                profile.protocol
            })
            .or_else(|| default_protocol_for_profile(&profile_name))
            .or(config.default_protocol)
            .unwrap_or(Protocol::Auto);

        if let Some(profile) = profile_config {
            run_configured_profile(
                &profile_name,
                profile,
                &extra_args,
                protocol,
                &cwd,
                max_diagnostics,
            )?
        }
        else {
            run_profile_with_limit(&profile_name, &extra_args, protocol, &cwd, max_diagnostics)?
        }
    };

    write_digest(&digest, format)?;
    enforce_diagnostic_gate(&digest, &expected_codes, fail_on_diagnostics)
}

/// Render one or more deterministic explanations.
///
/// # Contract
/// - requires: `command.source` is non-empty after clap parsing.
/// - ensures: writes one explanation block for the source alone or for each
///   requested code.
/// - fails: stdout write errors propagate.
/// - panics: debug assertion failure only if clap provides an empty explanation
///   source.
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
/// - requires: `command` came from clap and identifies one inspection action.
/// - ensures: writes the requested configuration detail to stdout.
/// - fails: configuration discovery or stdout write errors propagate.
/// - panics: none.
fn run_config(command: &ConfigCommand) -> Result<(), CliError>
{
    match command {
        | ConfigCommand::Paths => write_config_paths(),
        | ConfigCommand::Profiles(command) => write_config_profiles(command),
    }
}

/// Generate a completion script for the requested shell.
fn run_completions(command: &CompletionsCommand)
{
    let mut clap_command = Cli::command();
    let mut stdout = io::stdout().lock();

    generate(command.shell, &mut clap_command, "aifix", &mut stdout);
}

/// Print the configuration files aifix considered for the current directory.
///
/// # Contract
/// - requires: the current directory must be valid UTF-8.
/// - ensures: writes user and project configuration paths or `-` markers.
/// - fails: current directory, path discovery, or stdout write errors
///   propagate.
/// - panics: none.
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

/// Discover and render batch profile metadata for a requested working
/// directory.
///
/// # Contract
/// - requires: `command.cwd`, when present, is a UTF-8 path accepted by
///   configuration discovery.
/// - ensures: renders the catalog returned by the batch library in the
///   requested format and writes it to stdout with a trailing newline.
/// - fails: current directory, configuration discovery, catalog rendering, or
///   stdout write errors propagate.
/// - panics: none.
fn write_config_profiles(command: &ConfigProfilesCommand) -> Result<(), CliError>
{
    let cwd = match &command.cwd {
        | Some(path) => path.clone(),
        | None => current_utf8_dir()?,
    };
    let loaded_config = Config::discover(&cwd)?;
    let profiles = profile_catalog(&loaded_config.config, &cwd);
    let rendered = render_profile_catalog(&profiles, command.format.into())?;
    let mut stdout = io::stdout().lock();

    if rendered.ends_with('\n') {
        write!(stdout, "{rendered}")?;
    }
    else {
        writeln!(stdout, "{rendered}")?;
    }

    Ok(())
}

/// Return the actionable error used when callers pass profile-specific
/// arguments to `auto`.
///
/// # Contract
/// - requires: `config` is the already-discovered runtime configuration.
/// - ensures: names `auto`, explains why extra args are rejected, and lists
///   profiles callers can choose when they need profile-specific arguments.
/// - fails: allocation may abort through the global allocator; no recoverable
///   error is returned.
/// - panics: none.
fn auto_extra_args_message(config: &Config) -> String
{
    let profiles = available_profile_names(config).join(", ");

    format!(
        "`{AUTO_PROFILE}` does not accept extra arguments after `--` because \
         extra arguments are profile-specific. Use a named profile instead, \
         such as one of: {profiles}. Discover profiles with \
         `aifix config profiles --format json`."
    )
}

/// Write one explanation block to standard output.
///
/// # Contract
/// - requires: `stdout` is writable and `explanation` was built by the library.
/// - ensures: writes reference, status, summary, and a trailing blank line.
/// - fails: stdout write errors propagate.
/// - panics: debug assertion failure only if library explanations omit their
///   reference.
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
/// - requires: `input.raw` is `-` for stdin or a path readable by this process.
/// - ensures: returns the full input text without protocol interpretation.
/// - fails: stdin or filesystem read errors propagate.
/// - panics: debug assertion failure only if clap provides an empty non-stdin
///   input path.
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
/// - requires: `extra_args` came from clap after the batch `--` separator.
/// - ensures: returns every argument unchanged as UTF-8 and preserves order.
/// - fails: returns a CLI error naming the first argument whose OS bytes are
///   not valid UTF-8.
/// - panics: none.
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

/// Enforce CLI diagnostic-gate options after rendering the digest.
///
/// # Contract
/// - requires: `digest` was built from normalized diagnostics and
///   `expected_codes` contains user-provided code labels.
/// - ensures: returns success unless gate mode is active and at least one
///   diagnostic group has no expected code.
/// - fails: returns [`CliError::UnexpectedDiagnostics`] for unexpected groups.
/// - panics: none.
fn enforce_diagnostic_gate(
    digest: &Digest,
    expected_codes: &[String],
    fail_on_diagnostics: bool,
) -> Result<(), CliError>
{
    if !fail_on_diagnostics {
        return Ok(());
    }

    let count = unexpected_diagnostic_count(digest, expected_codes);
    if count == 0 {
        Ok(())
    }
    else {
        Err(CliError::UnexpectedDiagnostics { count })
    }
}

/// Count deduplicated diagnostics not covered by expected code labels.
///
/// # Contract
/// - requires: `digest.groups` was produced by digest construction.
/// - ensures: sums represented group counts whose code is absent from
///   `expected_codes`, treating code-less groups as unexpected.
/// - panics: none.
fn unexpected_diagnostic_count(
    digest: &Digest,
    expected_codes: &[String],
) -> usize
{
    digest
        .groups
        .iter()
        .filter(|group| {
            !group
                .code
                .as_deref()
                .is_some_and(|code| expected_codes.iter().any(|expected| expected == code))
        })
        .map(|group| group.count)
        .sum()
}

/// Write a rendered digest to standard output.
///
/// # Contract
/// - requires: `digest` is internally consistent and `format` is supported by
///   the renderer.
/// - ensures: writes the rendered digest and ensures it ends with a newline.
/// - fails: render or stdout write errors propagate.
/// - panics: debug assertion failure only if the renderer returns empty output.
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
/// - requires: the process has an accessible current directory.
/// - ensures: returns a camino UTF-8 path buffer.
/// - fails: current directory IO errors or non-UTF-8 paths propagate.
/// - panics: none.
fn current_utf8_dir() -> Result<Utf8PathBuf, CliError>
{
    let cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(cwd)
        .map_err(|path| CliError::NonUtf8CurrentDir(path.to_string_lossy().into_owned()))
}

/// Return a printable path, using `-` when that path was not discovered.
fn display_optional_path(path: Option<&Utf8Path>) -> &str
{
    path.map_or("-", Utf8Path::as_str)
}
