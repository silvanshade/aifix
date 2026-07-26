//! Configuration discovery for `aifix`.
//!
//! Discovery loads an optional user configuration followed by the nearest
//! regular-file project `aifix.toml`. User configuration defaults to an
//! XDG-style candidate (`$XDG_CONFIG_HOME/aifix/aifix.toml`, then
//! `$HOME/.config/aifix/aifix.toml`) and has no candidate when neither
//! environment variable is usable. Set `AIFIX_CONFIG_DIR_MODE` to
//! `platform-native` or `native` to opt into the platform-native
//! `directories::ProjectDirs` location instead. Unsupported or non-Unicode mode
//! values fail as configuration errors. Existing non-file config candidates are
//! rejected instead of skipped so malformed project state is visible. Project
//! configuration wins field-by-field so teams can override personal defaults
//! while inheriting unspecified values.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::env;
use std::fs;
use std::io;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use directories::ProjectDirs;
use serde::Deserialize;
use serde::Serialize;

use crate::error::AifixError;
use crate::model::OutputFormat;
use crate::model::Protocol;

/// Environment variable selecting the user config directory policy.
const CONFIG_DIR_MODE_ENV: &str = "AIFIX_CONFIG_DIR_MODE";
/// Config directory mode that selects XDG-style user config discovery.
const CONFIG_DIR_MODE_XDG: &str = "xdg";
/// Config directory mode that selects `directories::ProjectDirs`.
const CONFIG_DIR_MODE_PLATFORM_NATIVE: &str = "platform-native";
/// Short config directory mode alias for `directories::ProjectDirs`.
const CONFIG_DIR_MODE_NATIVE: &str = "native";
/// XDG base directory environment variable for user configuration.
const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";
/// Home directory environment variable used for the XDG fallback.
const HOME_ENV: &str = "HOME";
/// Application directory name used below config roots.
const APP_NAME: &str = "aifix";
/// XDG fallback config directory below `HOME`.
const XDG_CONFIG_DIR_NAME: &str = ".config";
/// Configuration file name used by user and project config discovery.
const CONFIG_FILE_NAME: &str = "aifix.toml";

/// Complete runtime configuration after user/project merging.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Config
{
    /// Default renderer used when the CLI does not receive `--format`.
    #[serde(default)]
    pub default_format: Option<OutputFormat>,
    /// Default input protocol used when the CLI does not receive `--protocol`.
    #[serde(default)]
    pub default_protocol: Option<Protocol>,
    /// Default maximum number of sample diagnostics per group.
    #[serde(default)]
    pub max_diagnostics: Option<usize>,
    /// Default maximum accepted bytes from each batch output stream.
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    /// Named batch profiles available to `aifix batch`.
    #[serde(default, alias = "profile")]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// LSP code-action settings for one batch profile.
///
/// Every field is optional so layered configuration can override one setting
/// without repeating the complete server contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CodeActionConfig
{
    /// Whether this profile participates in LSP code-action mutation.
    ///
    /// Set `false` in a higher-precedence layer to clear inherited support.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Language-server executable and arguments, using direct argv execution.
    #[serde(default, alias = "command")]
    pub argv: Option<Vec<String>>,
    /// LSP language identifier sent with opened source documents.
    #[serde(default)]
    pub language_id: Option<String>,
    /// Source-file extensions opened in the LSP session, without leading dots.
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    /// Code-action kinds eligible for automatic application.
    #[serde(default)]
    pub action_kinds: Option<Vec<String>>,
    /// Server command identifiers eligible for automatic execution.
    ///
    /// Commands are denied unless explicitly listed.
    #[serde(default)]
    pub allowed_commands: Option<Vec<String>>,
    /// Maximum mutation and diagnostic-refresh iterations.
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Overall timeout for one LSP request or diagnostic refresh.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Command configuration for one batch profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProfileConfig
{
    /// Executable and arguments used for this profile.
    #[serde(default, alias = "command")]
    pub argv: Option<Vec<String>>,
    /// Executable and arguments used for an opt-in native automatic-fix pass.
    ///
    /// Empty values fall back to a built-in fix command when the named profile
    /// provides one.
    #[serde(default, alias = "fix-command")]
    pub fix_argv: Option<Vec<String>>,
    /// Protocol used to parse this profile's output when not specified on CLI.
    #[serde(default)]
    pub protocol: Option<Protocol>,
    /// Protocol used to classify a nonzero native-fix result.
    ///
    /// Defaults to the profile's diagnostic protocol.
    #[serde(default)]
    pub fix_protocol: Option<Protocol>,
    /// Optional LSP code-action settings for this profile.
    #[serde(default, alias = "code-actions")]
    pub code_actions: Option<CodeActionConfig>,
    /// Renderer used for this profile when not specified on CLI.
    #[serde(default)]
    pub format: Option<OutputFormat>,
    /// Profile-specific sample cap.
    #[serde(default)]
    pub max_diagnostics: Option<usize>,
    /// Profile-specific maximum accepted bytes from each output stream.
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    /// Whether this configured profile participates in `batch auto`.
    #[serde(default)]
    pub auto: Option<bool>,
}

/// Configuration plus the paths that contributed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoadedConfig
{
    /// Merged configuration.
    pub config: Config,
    /// Candidate and discovered paths for reporting.
    pub paths: ConfigPaths,
}

/// Candidate and loaded configuration paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConfigPaths
{
    /// User-level config path when the selected path policy exposes one.
    pub user: Option<Utf8PathBuf>,
    /// Nearest project config found from the start directory upward.
    pub project: Option<Utf8PathBuf>,
    /// Paths that existed and were loaded, in merge order.
    pub loaded: Vec<Utf8PathBuf>,
}

/// Configuration merge and discovery behavior.
impl Config
{
    /// Discover and merge user and project configuration for `start`.
    ///
    /// User configuration uses the XDG-style candidate by default. Set
    /// `AIFIX_CONFIG_DIR_MODE` to `platform-native` or `native` to use the
    /// platform-native config directory instead.
    ///
    /// # Contract
    /// - requires: `start` names a UTF-8 path that may be either a file or a
    ///   directory.
    /// - ensures: returns a config where readable user settings are merged
    ///   first from the same candidate returned by [`config_paths`] and nearest
    ///   project settings override them field-by-field; absent user config is
    ///   skipped, including when XDG mode has neither usable `XDG_CONFIG_HOME`
    ///   nor `HOME`, while discovered non-file config paths are rejected with a
    ///   configuration error.
    /// - fails: returns an `AifixError` when the config-dir mode environment
    ///   value is unsupported or non-Unicode, the search directory cannot be
    ///   derived, a discovered config path is not a file, a config file cannot
    ///   be read, or TOML deserialization fails.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns an error when `AIFIX_CONFIG_DIR_MODE` is unsupported or
    /// non-Unicode, the config search directory cannot be derived, a discovered
    /// config path is not a file, a discovered config file cannot be read, or
    /// TOML decoding fails.
    #[inline]
    pub fn discover(start: &Utf8Path) -> Result<LoadedConfig, AifixError>
    {
        let paths = config_paths(start)?;
        let mut config = Self::default();
        let mut loaded = Vec::new();

        if let Some(path) = paths.user.as_ref()
            && let Some(user_config) = read_optional_config(path)?
        {
            config.merge(user_config);
            loaded.push(path.clone());
        }

        if let Some(path) = paths.project.as_ref() {
            config.merge(read_config(path)?);
            loaded.push(path.clone());
        }

        Ok(LoadedConfig {
            config,
            paths: ConfigPaths { loaded, ..paths },
        })
    }

    /// Merge `other` into this config, replacing only explicitly specified
    /// values.
    ///
    /// # Contract
    /// - requires: profile names in `other` are already decoded as valid UTF-8
    ///   map keys.
    /// - ensures: every explicitly present setting in `other` replaces this
    ///   config's matching setting, including empty argv and `false`; existing
    ///   profile fields are merged rather than dropped.
    /// - fails: none.
    /// - panics: none.
    fn merge(
        &mut self,
        other: Self,
    )
    {
        if other.default_format.is_some() {
            self.default_format = other.default_format;
        }
        if other.default_protocol.is_some() {
            self.default_protocol = other.default_protocol;
        }
        if other.max_diagnostics.is_some() {
            self.max_diagnostics = other.max_diagnostics;
        }
        if other.max_output_bytes.is_some() {
            self.max_output_bytes = other.max_output_bytes;
        }

        for (name, profile) in other.profiles {
            self.profiles
                .entry(name)
                .and_modify(|existing| existing.merge(profile.clone()))
                .or_insert(profile);
        }
    }
}

/// Profile-specific merge behavior.
impl ProfileConfig
{
    /// Merge `other` into this profile, replacing only explicitly specified
    /// values.
    ///
    /// # Contract
    /// - requires: a present, non-empty `other.argv` starts with the executable
    ///   the caller wants to run.
    /// - ensures: explicit profile fields in `other` replace this profile's
    ///   corresponding fields, including empty argv and `false`; omitted fields
    ///   keep their current values.
    /// - fails: none.
    /// - panics: none.
    fn merge(
        &mut self,
        other: Self,
    )
    {
        if other.argv.is_some() {
            self.argv = other.argv;
        }
        if other.fix_argv.is_some() {
            self.fix_argv = other.fix_argv;
        }
        if other.protocol.is_some() {
            self.protocol = other.protocol;
        }
        if other.fix_protocol.is_some() {
            self.fix_protocol = other.fix_protocol;
        }
        if let Some(other_code_actions) = other.code_actions {
            if let Some(code_actions) = self.code_actions.as_mut() {
                code_actions.merge(other_code_actions);
            }
            else {
                self.code_actions = Some(other_code_actions);
            }
        }
        if other.format.is_some() {
            self.format = other.format;
        }
        if other.max_diagnostics.is_some() {
            self.max_diagnostics = other.max_diagnostics;
        }
        if other.max_output_bytes.is_some() {
            self.max_output_bytes = other.max_output_bytes;
        }
        if other.auto.is_some() {
            self.auto = other.auto;
        }
    }
}

/// Code-action-specific merge behavior.
impl CodeActionConfig
{
    /// Merge explicitly present settings from `other`.
    fn merge(
        &mut self,
        other: Self,
    )
    {
        if other.enabled.is_some() {
            self.enabled = other.enabled;
        }
        if other.argv.is_some() {
            self.argv = other.argv;
        }
        if other.language_id.is_some() {
            self.language_id = other.language_id;
        }
        if other.extensions.is_some() {
            self.extensions = other.extensions;
        }
        if other.action_kinds.is_some() {
            self.action_kinds = other.action_kinds;
        }
        if other.allowed_commands.is_some() {
            self.allowed_commands = other.allowed_commands;
        }
        if other.max_iterations.is_some() {
            self.max_iterations = other.max_iterations;
        }
        if other.timeout_ms.is_some() {
            self.timeout_ms = other.timeout_ms;
        }
    }
}

/// Return candidate config paths for CLI reporting without loading files.
///
/// User configuration uses XDG-style discovery by default: non-empty
/// valid-UTF-8 `XDG_CONFIG_HOME` yields `$XDG_CONFIG_HOME/aifix/aifix.toml`;
/// otherwise absent or empty `XDG_CONFIG_HOME` with non-empty valid-UTF-8
/// `HOME` yields `$HOME/.config/aifix/aifix.toml`; otherwise no user candidate
/// is returned. Set `AIFIX_CONFIG_DIR_MODE` to `platform-native` or `native` to
/// use `directories::ProjectDirs` instead.
///
/// # Contract
/// - requires: `start` names a UTF-8 path that may be either a file or a
///   directory.
/// - ensures: returns the selected user config candidate, the nearest
///   regular-file project config if one exists, and an empty loaded list.
/// - fails: returns an `AifixError` when `AIFIX_CONFIG_DIR_MODE` is unsupported
///   or non-Unicode, `start` has no usable directory ancestor, candidate
///   metadata cannot be read, or a discovered `aifix.toml` exists but is not a
///   regular file.
/// - panics: none.
///
/// # Errors
/// Returns an error when `AIFIX_CONFIG_DIR_MODE` is unsupported or non-Unicode,
/// when `start` has no usable directory ancestor for project configuration
/// discovery, candidate metadata cannot be read, or a discovered project
/// `aifix.toml` path exists but is not a regular file.
#[inline]
pub fn config_paths(start: &Utf8Path) -> Result<ConfigPaths, AifixError>
{
    let user = user_config_path()?;
    let project = nearest_project_config(start)?;

    Ok(ConfigPaths {
        user,
        project,
        loaded: Vec::new(),
    })
}

/// Resolve the selected user-level configuration candidate.
///
/// # Contract
/// - requires: environment values may be absent or present with arbitrary
///   process-provided bytes.
/// - ensures: absent, empty, or `xdg` `AIFIX_CONFIG_DIR_MODE` selects the
///   XDG-style candidate; `platform-native` and `native` select the
///   `directories::ProjectDirs` candidate.
/// - fails: returns a configuration error when `AIFIX_CONFIG_DIR_MODE` is
///   present but not valid UTF-8 or not one of the supported values.
/// - panics: none.
fn user_config_path() -> Result<Option<Utf8PathBuf>, AifixError>
{
    match env::var(CONFIG_DIR_MODE_ENV) {
        | Ok(mode) if mode.is_empty() || mode == CONFIG_DIR_MODE_XDG => Ok(xdg_user_config_path()),
        | Ok(mode) if mode == CONFIG_DIR_MODE_PLATFORM_NATIVE || mode == CONFIG_DIR_MODE_NATIVE => {
            Ok(platform_native_user_config_path())
        },
        | Ok(mode) => Err(unsupported_config_dir_mode_error(&mode)),
        | Err(env::VarError::NotPresent) => Ok(xdg_user_config_path()),
        | Err(env::VarError::NotUnicode(_)) => Err(AifixError::config(format!(
            "{CONFIG_DIR_MODE_ENV} must be valid UTF-8"
        ))),
    }
}

/// Build the default XDG-style user configuration candidate.
///
/// # Contract
/// - requires: environment values may be absent, empty, or non-Unicode.
/// - ensures: returns `$XDG_CONFIG_HOME/aifix/aifix.toml` when
///   `XDG_CONFIG_HOME` is present, valid UTF-8, and non-empty; returns
///   `$HOME/.config/aifix/aifix.toml` when `XDG_CONFIG_HOME` is absent or empty
///   and `HOME` is present, valid UTF-8, and non-empty; otherwise returns
///   `None`.
/// - fails: none.
/// - panics: none.
fn xdg_user_config_path() -> Option<Utf8PathBuf>
{
    match env::var(XDG_CONFIG_HOME_ENV) {
        | Ok(base) if !base.is_empty() => {
            return Some(
                Utf8PathBuf::from(base)
                    .join(APP_NAME)
                    .join(CONFIG_FILE_NAME),
            );
        },
        | Ok(_) | Err(env::VarError::NotPresent) => {},
        | Err(env::VarError::NotUnicode(_)) => return None,
    }

    match env::var(HOME_ENV) {
        | Ok(base) if !base.is_empty() => Some(
            Utf8PathBuf::from(base)
                .join(XDG_CONFIG_DIR_NAME)
                .join(APP_NAME)
                .join(CONFIG_FILE_NAME),
        ),
        | Ok(_) | Err(_) => None,
    }
}

/// Build the platform-native user configuration candidate.
///
/// # Contract
/// - requires: platform directory discovery may or may not expose a UTF-8
///   config directory.
/// - ensures: returns the existing `directories::ProjectDirs` config candidate
///   when it can be represented as UTF-8, otherwise `None`.
/// - fails: none.
/// - panics: none.
fn platform_native_user_config_path() -> Option<Utf8PathBuf>
{
    let dirs = ProjectDirs::from("dev", APP_NAME, APP_NAME)?;

    Utf8PathBuf::from_path_buf(dirs.config_dir().join(CONFIG_FILE_NAME)).ok()
}

/// Build the stable error used for unsupported config-directory modes.
///
/// # Contract
/// - requires: `mode` is the unsupported UTF-8 value read from
///   `AIFIX_CONFIG_DIR_MODE`.
/// - ensures: returns an [`AifixError::Config`] identifying the variable, the
///   unsupported value, and the supported values.
/// - fails: allocation may abort through the global allocator; no recoverable
///   error is returned.
/// - panics: none.
fn unsupported_config_dir_mode_error(mode: &str) -> AifixError
{
    AifixError::config(format!(
        "unsupported {CONFIG_DIR_MODE_ENV} value `{mode}`; expected `{CONFIG_DIR_MODE_XDG}`, `{CONFIG_DIR_MODE_PLATFORM_NATIVE}`, or `{CONFIG_DIR_MODE_NATIVE}`"
    ))
}

/// Read and deserialize an optional TOML config file.
///
/// # Contract
/// - requires: `path` names the intended UTF-8 TOML configuration file.
/// - ensures: returns `None` only when `path` is absent; returns decoded config
///   for regular files. Existing non-files are rejected instead of being
///   treated as absent so user mistakes are visible.
/// - fails: returns an I/O error for unreadable files, a configuration error
///   for existing non-files, or a TOML error for invalid config text.
/// - panics: none.
///
/// # Errors
/// Returns an I/O error when `path` cannot be read, a configuration error when
/// `path` exists but is not a regular file, or a TOML decoding error when the
/// file contents are invalid.
fn read_optional_config(path: &Utf8Path) -> Result<Option<Config>, AifixError>
{
    match read_config_text(path)? {
        | Some(text) => toml::from_str(&text)
            .map(Some)
            .map_err(AifixError::TomlDeserialize),
        | None => Ok(None),
    }
}

/// Read and deserialize one TOML config file.
///
/// # Contract
/// - requires: `path` names an existing UTF-8 TOML configuration file.
/// - ensures: returns the decoded configuration without applying defaults
///   beyond Serde field defaults.
/// - fails: returns an I/O error for unreadable files, a configuration error
///   for existing non-files, or a TOML error for invalid configuration text.
/// - panics: none.
///
/// # Errors
/// Returns an I/O error when `path` cannot be read, a configuration error when
/// `path` exists but is not a regular file, or a TOML decoding error when the
/// file contents are invalid.
fn read_config(path: &Utf8Path) -> Result<Config, AifixError>
{
    let text = read_config_text(path)?.ok_or_else(|| {
        AifixError::config(format!(
            "configuration file disappeared before read: {path}"
        ))
    })?;
    toml::from_str(&text).map_err(AifixError::TomlDeserialize)
}

/// Read optional config text without an `exists`-then-read check.
///
/// # Contract
/// - requires: `path` names the intended UTF-8 TOML configuration file.
/// - ensures: returns `None` only when metadata or read reports `NotFound`;
///   returns file text for readable regular files; rejects existing non-files
///   with a stable configuration error.
/// - fails: returns configuration errors for non-files and I/O errors for
///   unreadable files other than absence.
/// - panics: none.
///
/// # Errors
/// Returns a configuration error when `path` exists but is not a regular file,
/// or an I/O error when reading `path` fails for any reason except absence.
fn read_config_text(path: &Utf8Path) -> Result<Option<String>, AifixError>
{
    let metadata = match fs::metadata(path) {
        | Ok(metadata) => metadata,
        | Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        | Err(source) => return Err(AifixError::io_path(path.to_owned(), source)),
    };
    if !metadata.is_file() {
        return Err(non_file_config_error(path));
    }

    match fs::read_to_string(path) {
        | Ok(text) => Ok(Some(text)),
        | Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        | Err(source) if source.kind() == io::ErrorKind::IsADirectory => {
            Err(non_file_config_error(path))
        },
        | Err(source) => Err(AifixError::io_path(path.to_owned(), source)),
    }
}

/// Build the stable error used when a config candidate is not a regular file.
///
/// # Contract
/// - requires: caller observed that `path` exists but is not a regular file.
/// - ensures: returns an [`AifixError::Config`] whose message identifies the
///   path and chosen behavior: reject rather than skip.
/// - fails: allocation may abort through the global allocator; no recoverable
///   error is returned.
/// - panics: none.
fn non_file_config_error(path: &Utf8Path) -> AifixError
{
    AifixError::config(format!(
        "configuration path exists but is not a regular file and will not be skipped: {path}"
    ))
}

/// Locate the nearest `aifix.toml` walking upward from `start`.
///
/// # Contract
/// - requires: `start` names a UTF-8 file or directory path.
/// - ensures: returns the first regular-file `aifix.toml` found from the
///   normalized start directory toward the filesystem root, or `None` when
///   every candidate is absent.
/// - fails: returns an `AifixError` when no search directory can be derived,
///   candidate metadata cannot be read, or an `aifix.toml` candidate exists but
///   is not a regular file.
/// - panics: none.
///
/// # Errors
/// Returns an error when `start` cannot be normalized into a search directory,
/// candidate metadata cannot be read, or an existing `aifix.toml` candidate is
/// not a regular file.
fn nearest_project_config(start: &Utf8Path) -> Result<Option<Utf8PathBuf>, AifixError>
{
    let mut current = normalize_start_dir(start)?;

    loop {
        let candidate = current.join("aifix.toml");
        match fs::metadata(&candidate) {
            | Ok(metadata) if metadata.is_file() => return Ok(Some(candidate)),
            | Ok(_) => return Err(non_file_config_error(&candidate)),
            | Err(source) if source.kind() == io::ErrorKind::NotFound => {},
            | Err(source) => return Err(AifixError::io_path(candidate, source)),
        }

        if !current.pop() {
            return Ok(None);
        }
    }
}

/// Resolve `start` to a directory for upward project config search.
///
/// # Contract
/// - requires: `start` names a UTF-8 path provided by the caller.
/// - ensures: returns `start` unchanged when filesystem metadata says it is a
///   directory; otherwise returns its UTF-8 parent directory. Missing paths are
///   treated as file-like starts so callers can search from a planned file's
///   parent.
/// - fails: returns a configuration error when a non-directory path has no
///   parent directory, or an I/O error when existing path metadata cannot be
///   read.
/// - panics: none.
///
/// # Errors
/// Returns an error when a non-directory `start` has no parent directory to use
/// for project configuration discovery, or when existing path metadata cannot
/// be read.
fn normalize_start_dir(start: &Utf8Path) -> Result<Utf8PathBuf, AifixError>
{
    let use_parent = match fs::metadata(start) {
        | Ok(metadata) => !metadata.is_dir(),
        | Err(source) if source.kind() == io::ErrorKind::NotFound => true,
        | Err(source) => return Err(AifixError::io_path(start.to_owned(), source)),
    };

    if !use_parent {
        return Ok(start.to_owned());
    }

    start.parent().map(Utf8Path::to_owned).ok_or_else(|| {
        AifixError::config(format!(
            "cannot determine configuration search directory for {start}"
        ))
    })
}
