//! Configuration discovery for `aifix`.
//!
//! Discovery loads an optional user configuration followed by the nearest
//! regular-file project `aifix.toml`.  Existing non-file config candidates are
//! rejected instead of skipped so malformed project state is visible. Project
//! configuration wins field-by-field so teams can override personal defaults
//! while inheriting unspecified values.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
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
    /// Named batch profiles available to `aifix batch`.
    #[serde(default, alias = "profile")]
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// Command configuration for one batch profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProfileConfig
{
    /// Executable and arguments used for this profile.
    #[serde(default, alias = "command")]
    pub argv: Vec<String>,
    /// Protocol used to parse this profile's output when not specified on CLI.
    #[serde(default)]
    pub protocol: Option<Protocol>,
    /// Renderer used for this profile when not specified on CLI.
    #[serde(default)]
    pub format: Option<OutputFormat>,
    /// Profile-specific sample cap.
    #[serde(default)]
    pub max_diagnostics: Option<usize>,
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
    /// User-level config path when the platform exposes one.
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
    /// # Contract
    /// - requires: `start` names a UTF-8 path that may be either a file or a
    ///   directory.
    /// - ensures: returns a config where readable user settings are merged
    ///   first and nearest project settings override them field-by-field;
    ///   absent user config is skipped, while discovered non-file config paths
    ///   are rejected with a configuration error.
    /// - fails: returns an `AifixError` when the search directory cannot be
    ///   derived, a discovered config path is not a file, a config file cannot
    ///   be read, or TOML deserialization fails.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns an error when the config search directory cannot be derived, a
    /// discovered config path is not a file, a discovered config file cannot be
    /// read, or TOML decoding fails.
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
    /// - ensures: every non-empty setting in `other` replaces this config's
    ///   matching setting; existing profile fields are merged rather than
    ///   dropped.
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
    /// - requires: `other.argv`, when non-empty, starts with the executable the
    ///   caller wants to run.
    /// - ensures: explicit profile fields in `other` replace this profile's
    ///   corresponding fields; omitted fields keep their current values.
    /// - fails: none.
    /// - panics: debug builds may panic if a non-empty incoming argv becomes
    ///   empty after assignment.
    fn merge(
        &mut self,
        other: Self,
    )
    {
        if !other.argv.is_empty() {
            self.argv = other.argv;
            debug_assert!(
                !self.argv.is_empty(),
                "non-empty profile argv must remain non-empty after merge"
            );
        }
        if other.protocol.is_some() {
            self.protocol = other.protocol;
        }
        if other.format.is_some() {
            self.format = other.format;
        }
        if other.max_diagnostics.is_some() {
            self.max_diagnostics = other.max_diagnostics;
        }
    }
}

/// Return candidate config paths for CLI reporting without loading files.
///
/// # Contract
/// - requires: `start` names a UTF-8 path that may be either a file or a
///   directory.
/// - ensures: returns the platform user config candidate, the nearest
///   regular-file project config if one exists, and an empty loaded list.
/// - fails: returns an `AifixError` when `start` has no usable directory
///   ancestor, candidate metadata cannot be read, or a discovered `aifix.toml`
///   exists but is not a regular file.
/// - panics: none.
///
/// # Errors
/// Returns an error when `start` has no usable directory ancestor for project
/// configuration discovery, candidate metadata cannot be read, or a discovered
/// project `aifix.toml` path exists but is not a regular file.
#[inline]
pub fn config_paths(start: &Utf8Path) -> Result<ConfigPaths, AifixError>
{
    let user = ProjectDirs::from("dev", "aifix", "aifix")
        .and_then(|dirs| Utf8PathBuf::from_path_buf(dirs.config_dir().join("aifix.toml")).ok());
    let project = nearest_project_config(start)?;

    Ok(ConfigPaths {
        user,
        project,
        loaded: Vec::new(),
    })
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
