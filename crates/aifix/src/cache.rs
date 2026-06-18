//! Project-local diagnostic cache persistence and fix replay.
//!
//! The cache lives at `<projectRoot>/.aifix/diagnostics.json` and stores stable
//! diagnostic signatures, cached patch text, and shape metrics in deterministic
//! JSON.  It is intentionally independent from the MCP transport so CLI, tests,
//! and future embedding callers can share the same behavior.

use alloc::borrow::ToOwned as _;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use std::io;
use std::process::Command;
use std::process::Stdio;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use serde::Deserialize;
use serde::Serialize;

use crate::adapter::parse_diagnostics;
use crate::error::AifixError;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Protocol;
use crate::model::Severity;
use crate::signature::DiagnosticSignature;

/// Cache schema version written to disk.
///
/// # Contract
/// Preconditions: callers compare this with
/// [`DiagnosticCache::schema_version`]. Postconditions: persisted cache files
/// use this exact version. Failure modes: none. Panics: none.
pub const CACHE_SCHEMA_VERSION: u8 = 1;

/// Project-relative cache file location.
///
/// # Contract
/// Preconditions: callers append this to a UTF-8 project root. Postconditions:
/// resolves to `.aifix/diagnostics.json`. Failure modes: none. Panics: none.
pub const CACHE_FILE_RELATIVE_PATH: &str = ".aifix/diagnostics.json";

/// Cache behavior for replaying stored patches.
///
/// # Contract
/// Preconditions: callers choose the mode from validated MCP or CLI input.
/// Postconditions: `Suggest` never invokes git, `DryRun` invokes `git apply
/// --check`, and `Apply` checks then applies. Failure modes: process failures
/// are returned by replay helpers. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayMode
{
    /// Return matching patch text without invoking git.
    Suggest,
    /// Verify each matching patch with `git apply --check`.
    DryRun,
    /// Verify each matching patch then apply it with `git apply`.
    Apply,
}

impl ReplayMode
{
    /// Return the stable mode spelling.
    ///
    /// # Contract
    /// Preconditions: `self` is any replay mode. Postconditions: returns the
    /// kebab-case spelling accepted by [`FromStr`]. Failure modes: none.
    /// Panics: none.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str
    {
        match self {
            | Self::Suggest => "suggest",
            | Self::DryRun => "dry-run",
            | Self::Apply => "apply",
        }
    }
}

impl fmt::Display for ReplayMode
{
    /// Format the mode with the stable spelling.
    ///
    /// # Contract
    /// Preconditions: `f` is writable. Postconditions: writes
    /// [`ReplayMode::as_str`]. Failure modes: returns formatter errors.
    /// Panics: none.
    ///
    /// # Errors
    /// Returns formatter errors when writing the mode string fails.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        f.write_str(self.as_str())
    }
}

impl core::str::FromStr for ReplayMode
{
    type Err = AifixError;

    /// Parse a replay mode spelling.
    ///
    /// # Contract
    /// Preconditions: `s` is caller-supplied text. Postconditions: returns the
    /// corresponding mode for supported spellings. Failure modes: invalid input
    /// returns [`AifixError::InvalidArgument`]. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when `s` is not a supported
    /// replay mode spelling.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        match s.trim() {
            | "suggest" => Ok(Self::Suggest),
            | "dry-run" | "dry_run" => Ok(Self::DryRun),
            | "apply" => Ok(Self::Apply),
            | other => Err(AifixError::invalid_argument(format!(
                "unknown fix replay mode `{other}`"
            ))),
        }
    }
}

/// Persistent project-local diagnostic cache.
///
/// # Contract
/// Preconditions: values are loaded with [`load_cache`] or constructed with
/// [`DiagnosticCache::default`]. Postconditions: maps serialize in
/// deterministic key order. Failure modes: serialization and validation happen
/// in load/save helpers. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticCache
{
    /// On-disk schema version for migration checks.
    pub schema_version: u8,
    /// Diagnostic signatures already emitted for this project.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub seen: BTreeMap<String, SeenDiagnostic>,
    /// Cached patch text keyed by diagnostic signature string.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fixes: BTreeMap<String, CachedFix>,
    /// Aggregated diagnostic-shape metrics.
    #[serde(default)]
    pub metrics: DiagnosticMetrics,
}

impl Default for DiagnosticCache
{
    /// Construct an empty schema-version-one cache.
    ///
    /// # Contract
    /// Preconditions: none. Postconditions: all maps are empty and schema
    /// version is current. Failure modes: none. Panics: none.
    #[inline]
    fn default() -> Self
    {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            seen: BTreeMap::new(),
            fixes: BTreeMap::new(),
            metrics: DiagnosticMetrics::default(),
        }
    }
}

impl DiagnosticCache
{
    /// Validate this cache's schema version.
    ///
    /// # Contract
    /// Preconditions: `self` was loaded or built by a caller. Postconditions:
    /// returns `Ok(())` only for supported schema version one. Failure modes:
    /// unsupported versions return [`AifixError::Config`]. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::Config`] when the cache schema version is not
    /// supported by this implementation.
    #[inline]
    pub fn validate(&self) -> Result<(), AifixError>
    {
        if self.schema_version == CACHE_SCHEMA_VERSION {
            return Ok(());
        }

        Err(AifixError::config(format!(
            "unsupported diagnostics cache schema version {}",
            self.schema_version
        )))
    }

    /// Filter diagnostics not previously seen by this cache.
    ///
    /// New signatures are recorded in `seen` before returning. Repeating the
    /// same diagnostics against the same cache therefore returns an empty
    /// vector on the second call.
    ///
    /// # Contract
    /// Preconditions: diagnostics are normalized. Postconditions: unseen
    /// diagnostics are returned in input order and recorded by stable
    /// signature. Failure modes: none. Panics: none.
    #[must_use]
    #[inline]
    pub fn filter_unseen_diagnostics(
        &mut self,
        diagnostics: &[Diagnostic],
    ) -> Vec<Diagnostic>
    {
        let mut unseen = Vec::new();
        for diagnostic in diagnostics {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            match self.seen.get_mut(signature.as_str()) {
                | Some(seen) => {
                    seen.seen_count = seen.seen_count.saturating_add(1);
                },
                | None => {
                    self.seen
                        .insert(signature, SeenDiagnostic::from_diagnostic(diagnostic));
                    unseen.push(diagnostic.clone());
                },
            }
        }
        unseen
    }

    /// Record shape metrics for every diagnostic supplied by the caller.
    ///
    /// # Contract
    /// Preconditions: diagnostics are normalized. Postconditions: aggregate
    /// counts by source, severity, code, and signature are incremented once per
    /// diagnostic. Failure modes: none. Panics: none.
    #[inline]
    pub fn record_metrics(
        &mut self,
        diagnostics: &[Diagnostic],
    )
    {
        self.metrics.record_diagnostics(diagnostics);
    }

    /// Store a cached fix for one diagnostic.
    ///
    /// # Contract
    /// Preconditions: `patch` is non-empty git-apply-compatible patch text and
    /// `diagnostic` is normalized. Postconditions: replaces any fix for the
    /// diagnostic's stable signature with the supplied patch and summary.
    /// Failure modes: empty patches return [`AifixError::InvalidArgument`].
    /// Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when the patch text is empty or
    /// whitespace only.
    #[inline]
    pub fn record_fix_for_diagnostic(
        &mut self,
        diagnostic: &Diagnostic,
        patch: String,
        note: Option<String>,
    ) -> Result<String, AifixError>
    {
        let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
        self.record_fix_entry(
            signature.clone(),
            patch,
            note,
            Some(DiagnosticSummary::from_diagnostic(diagnostic)),
        )?;
        Ok(signature)
    }

    /// Store a cached fix for a prevalidated signature string.
    ///
    /// # Contract
    /// Preconditions: `signature` has the documented `aifix-v1` shape and
    /// `patch` is non-empty git-apply-compatible text. Postconditions: replaces
    /// any fix for the canonical signature with the supplied patch. Failure
    /// modes: invalid signatures or empty patches return typed errors. Panics:
    /// none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when the signature is malformed
    /// or the patch text is empty or whitespace only.
    #[inline]
    pub fn record_fix_for_signature(
        &mut self,
        signature: &str,
        patch: String,
        note: Option<String>,
    ) -> Result<String, AifixError>
    {
        let canonical = DiagnosticSignature::canonical_key(signature)?;
        self.record_fix_entry(canonical.clone(), patch, note, None)?;
        Ok(canonical)
    }

    /// Find cached fixes for diagnostics without mutating use counts.
    ///
    /// # Contract
    /// Preconditions: diagnostics are normalized. Postconditions: returns fixes
    /// in diagnostic input order when a signature has a cached patch. Failure
    /// modes: none. Panics: none.
    #[must_use]
    #[inline]
    pub fn cached_fixes_for_diagnostics(
        &self,
        diagnostics: &[Diagnostic],
    ) -> Vec<CachedFixMatch>
    {
        let mut matches = Vec::new();
        for diagnostic in diagnostics {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            if let Some(fix) = self.fixes.get(signature.as_str()) {
                matches.push(CachedFixMatch {
                    signature,
                    fix: fix.clone(),
                });
            }
        }
        matches
    }

    /// Find cached fixes for diagnostics and increment use counts.
    ///
    /// # Contract
    /// Preconditions: diagnostics are normalized. Postconditions: returns fixes
    /// in diagnostic input order and increments each matched fix use count
    /// once. Failure modes: none. Panics: none.
    #[must_use]
    #[inline]
    pub fn take_cached_fixes_for_diagnostics(
        &mut self,
        diagnostics: &[Diagnostic],
    ) -> Vec<CachedFixMatch>
    {
        let mut matches = Vec::new();
        for diagnostic in diagnostics {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            if let Some(fix) = self.fixes.get_mut(signature.as_str()) {
                fix.use_count = fix.use_count.saturating_add(1);
                matches.push(CachedFixMatch {
                    signature,
                    fix: fix.clone(),
                });
            }
        }
        matches
    }

    /// Render deterministic Markdown guidance from current metrics.
    ///
    /// # Contract
    /// Preconditions: metrics were recorded through cache helpers or loaded
    /// from disk. Postconditions: returns deterministic Markdown ordered by map
    /// key for agents to understand repeated diagnostic shapes. Failure modes:
    /// allocation may abort through the global allocator. Panics: none.
    #[must_use]
    #[inline]
    pub fn render_guidance_markdown(&self) -> String
    {
        render_metrics_guidance_markdown(&self.metrics)
    }

    /// Insert a cached fix after validation.
    ///
    /// # Contract
    /// Preconditions: `signature` is canonical and `summary` matches it when
    /// present. Postconditions: the fix map contains the new patch entry.
    /// Failure modes: empty or whitespace-only patches return typed errors.
    /// Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when the patch text is empty or
    /// whitespace only.
    fn record_fix_entry(
        &mut self,
        signature: String,
        patch: String,
        note: Option<String>,
        summary: Option<DiagnosticSummary>,
    ) -> Result<(), AifixError>
    {
        if patch.trim().is_empty() {
            return Err(AifixError::invalid_argument(
                "cached fix patch must not be empty",
            ));
        }
        self.fixes.insert(signature, CachedFix {
            patch,
            note,
            summary,
            use_count: 0,
        });
        Ok(())
    }
}

/// Compact diagnostic metadata stored alongside seen entries and fixes.
///
/// # Contract
/// Preconditions: values are derived from normalized diagnostics.
/// Postconditions: raw payloads, spans, and suggestions are intentionally
/// omitted. Failure modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticSummary
{
    /// Diagnostic source tool or protocol family.
    pub source: String,
    /// Structured diagnostic code when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human-readable normalized diagnostic message.
    pub message: String,
    /// Normalized severity spelling.
    pub severity: String,
}

impl DiagnosticSummary
{
    /// Build a summary from one diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` is normalized. Postconditions: summary
    /// contains only small semantic fields and excludes raw payloads. Failure
    /// modes: none except allocator abort. Panics: none.
    #[must_use]
    #[inline]
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        Self {
            source: diagnostic.source.clone(),
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            severity: diagnostic.severity.as_str().to_owned(),
        }
    }
}

/// Persistent record for a previously emitted diagnostic.
///
/// # Contract
/// Preconditions: values are created from normalized diagnostics.
/// Postconditions: repeated suppression counts are retained without timestamps.
/// Failure modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenDiagnostic
{
    /// Small semantic summary for humans inspecting the cache.
    pub summary: DiagnosticSummary,
    /// Number of times this signature has been observed through dedupe.
    pub seen_count: usize,
}

impl SeenDiagnostic
{
    /// Build a seen record for the first observation of a diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` is normalized. Postconditions: `seen_count`
    /// is one and summary excludes raw payloads. Failure modes: allocator abort
    /// only. Panics: none.
    #[must_use]
    #[inline]
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        Self {
            summary: DiagnosticSummary::from_diagnostic(diagnostic),
            seen_count: 1,
        }
    }
}

/// Cached rerere-style fix data for one diagnostic signature.
///
/// # Contract
/// Preconditions: `patch` is non-empty git patch text. Postconditions: patch
/// text and optional note survive JSON round trips. Failure modes: none while
/// held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFix
{
    /// Patch text to feed directly to `git apply`.
    pub patch: String,
    /// Optional human note explaining why the patch is relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Optional diagnostic summary captured when recorded from a diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<DiagnosticSummary>,
    /// Number of replay lookups that selected this fix.
    pub use_count: usize,
}

/// Aggregated diagnostic-shape metrics.
///
/// # Contract
/// Preconditions: metrics are incremented through
/// [`DiagnosticMetrics::record_diagnostics`]. Postconditions: all maps use
/// deterministic key order. Failure modes: none while held as a value. Panics:
/// none.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticMetrics
{
    /// Total diagnostics recorded into metrics.
    pub total: usize,
    /// Counts by normalized source name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_source: BTreeMap<String, usize>,
    /// Counts by severity spelling.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_severity: BTreeMap<String, usize>,
    /// Counts by structured code, or `<none>` when absent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_code: BTreeMap<String, usize>,
    /// Counts by stable diagnostic signature.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_signature: BTreeMap<String, usize>,
}

impl DiagnosticMetrics
{
    /// Record metrics for a normalized diagnostic slice.
    ///
    /// # Contract
    /// Preconditions: diagnostics are normalized. Postconditions: source,
    /// severity, code, and signature maps are incremented once per diagnostic.
    /// Failure modes: allocator abort only. Panics: none.
    #[inline]
    pub fn record_diagnostics(
        &mut self,
        diagnostics: &[Diagnostic],
    )
    {
        for diagnostic in diagnostics {
            self.total = self.total.saturating_add(1);
            *self.by_source.entry(diagnostic.source.clone()).or_default() += 1;
            *self
                .by_severity
                .entry(severity_key(diagnostic.severity))
                .or_default() += 1;
            *self
                .by_code
                .entry(code_key(diagnostic.code.as_deref()))
                .or_default() += 1;
            *self
                .by_signature
                .entry(DiagnosticSignature::from_diagnostic(diagnostic).as_key())
                .or_default() += 1;
        }
    }
}

/// One cached fix match returned to replay callers.
///
/// # Contract
/// Preconditions: values are selected from a cache fix map. Postconditions:
/// signature and fix data are cloned for stable reporting. Failure modes: none
/// while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFixMatch
{
    /// Signature that selected this cached fix.
    pub signature: String,
    /// Cached fix selected for the signature.
    pub fix: CachedFix,
}

/// Result of a cached fix replay operation.
///
/// # Contract
/// Preconditions: constructed by [`replay_cached_fixes`]. Postconditions:
/// `matches` preserves diagnostic order, `checked` counts successful `git apply
/// --check` calls, and `applied` counts successful `git apply` calls. Failure
/// modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayResult
{
    /// Replay mode requested by the caller.
    pub mode: ReplayMode,
    /// Matching cached fixes in diagnostic input order.
    pub matches: Vec<CachedFixMatch>,
    /// Number of patches verified by `git apply --check`.
    pub checked: usize,
    /// Number of patches applied by `git apply`.
    pub applied: usize,
}

/// Resolve the project-local diagnostics cache path.
///
/// # Contract
/// Preconditions: `project_root`, when provided, is a UTF-8 project directory.
/// Postconditions: returns `<projectRoot>/.aifix/diagnostics.json`, using the
/// current directory when no root is supplied. Failure modes: non-UTF-8 current
/// directories return [`AifixError::Utf8`]; current-dir IO failures return
/// [`AifixError::Io`]. Panics: none.
///
/// # Errors
/// Returns [`AifixError::Io`] when the current directory cannot be read and
/// [`AifixError::Utf8`] when that directory is not valid UTF-8.
#[inline]
pub fn resolve_cache_path(project_root: Option<&Utf8Path>) -> Result<Utf8PathBuf, AifixError>
{
    let root = match project_root {
        | Some(path) => path.to_path_buf(),
        | None => current_utf8_dir()?,
    };
    Ok(root.join(CACHE_FILE_RELATIVE_PATH))
}

/// Load a diagnostics cache, treating a missing file as an empty cache.
///
/// # Contract
/// Preconditions: `project_root`, when provided, is a UTF-8 project directory.
/// Postconditions: returns a schema-version-one cache, empty when the file does
/// not exist. Failure modes: IO, JSON, or unsupported schema versions return
/// typed errors. Panics: none.
///
/// # Errors
/// Returns IO errors for unreadable cache files, JSON errors for malformed
/// cache contents, and config errors for unsupported schema versions.
#[inline]
pub fn load_cache(project_root: Option<&Utf8Path>) -> Result<DiagnosticCache, AifixError>
{
    let path = resolve_cache_path(project_root)?;
    match std::fs::read_to_string(path.as_std_path()) {
        | Ok(contents) => {
            let cache: DiagnosticCache = serde_json::from_str(contents.as_str())?;
            cache.validate()?;
            Ok(cache)
        },
        | Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DiagnosticCache::default()),
        | Err(error) => Err(AifixError::io_path(path, error)),
    }
}

/// Save a diagnostics cache, creating the `.aifix` directory when needed.
///
/// # Contract
/// Preconditions: `cache` has a supported schema version and `project_root`,
/// when supplied, is a UTF-8 project directory. Postconditions: writes pretty
/// deterministic JSON to the project-local cache file. Failure modes: IO, JSON,
/// or unsupported schema versions return typed errors. Panics: none.
///
/// # Errors
/// Returns config errors for unsupported schemas or invalid cache paths, IO
/// errors while creating directories or writing files, and JSON serialization
/// errors for cache encoding failures.
#[inline]
pub fn save_cache(
    project_root: Option<&Utf8Path>,
    cache: &DiagnosticCache,
) -> Result<(), AifixError>
{
    cache.validate()?;
    let path = resolve_cache_path(project_root)?;
    let Some(parent) = path.parent()
    else {
        return Err(AifixError::config(format!(
            "diagnostics cache path `{path}` has no parent directory"
        )));
    };
    std::fs::create_dir_all(parent.as_std_path())
        .map_err(|source| AifixError::io_path(parent.to_path_buf(), source))?;
    let rendered = serde_json::to_string_pretty(cache)?;
    std::fs::write(path.as_std_path(), rendered).map_err(|source| AifixError::io_path(path, source))
}

/// Resolve the project-local diagnostics cache path from an explicit root.
///
/// # Contract
/// Preconditions: `project_root` is a UTF-8 project directory. Postconditions:
/// returns `<projectRoot>/.aifix/diagnostics.json` without touching the
/// filesystem. Failure modes: none. Panics: none.
#[must_use]
#[inline]
pub fn diagnostic_cache_path(project_root: &Utf8Path) -> Utf8PathBuf
{
    project_root.join(CACHE_FILE_RELATIVE_PATH)
}

/// Load a diagnostics cache from an explicit project root.
///
/// # Contract
/// Preconditions: `project_root` is a UTF-8 project directory. Postconditions:
/// missing cache files load as empty schema-version-one caches. Failure modes:
/// IO, JSON, or schema errors return typed errors. Panics: none.
///
/// # Errors
/// Returns IO errors for unreadable cache files, JSON errors for malformed
/// cache contents, and config errors for unsupported schema versions.
#[inline]
pub fn load_diagnostic_cache(project_root: &Utf8Path) -> Result<DiagnosticCache, AifixError>
{
    load_cache(Some(project_root))
}

/// Save a diagnostics cache to an explicit project root.
///
/// # Contract
/// Preconditions: `project_root` is a UTF-8 project directory and `cache` has a
/// supported schema version. Postconditions: `.aifix` is created when missing
/// and deterministic JSON is written. Failure modes: IO, JSON, or schema errors
/// return typed errors. Panics: none.
///
/// # Errors
/// Returns config errors for unsupported schemas or invalid cache paths, IO
/// errors while creating directories or writing files, and JSON serialization
/// errors for cache encoding failures.
#[inline]
pub fn save_diagnostic_cache(
    project_root: &Utf8Path,
    cache: &DiagnosticCache,
) -> Result<(), AifixError>
{
    save_cache(Some(project_root), cache)
}

/// Filter unseen diagnostics through an existing cache.
///
/// # Contract
/// Preconditions: diagnostics are normalized. Postconditions: returned
/// diagnostics were absent from `cache` and are now recorded in it. Failure
/// modes: none. Panics: none.
#[must_use]
#[inline]
pub fn filter_unseen_diagnostics(
    cache: &mut DiagnosticCache,
    diagnostics: &[Diagnostic],
) -> Vec<Diagnostic>
{
    cache.filter_unseen_diagnostics(diagnostics)
}

/// Record diagnostic-shape metrics in an existing cache.
///
/// # Contract
/// Preconditions: diagnostics are normalized. Postconditions: metric counts are
/// incremented once per diagnostic. Failure modes: none. Panics: none.
#[inline]
pub fn record_diagnostic_metrics(
    cache: &mut DiagnosticCache,
    diagnostics: &[Diagnostic],
)
{
    cache.record_metrics(diagnostics);
}

/// Record a cached fix by diagnostic or signature in an existing cache.
///
/// # Contract
/// Preconditions: either `diagnostic` or `signature` is supplied and `patch` is
/// non-empty. Postconditions: the fix entry is stored and the canonical
/// signature is returned. Failure modes: invalid arguments return typed errors.
/// Panics: none.
///
/// # Errors
/// Returns [`AifixError::InvalidArgument`] when neither a diagnostic nor a
/// signature is provided, when the signature is malformed, or when the patch is
/// empty or whitespace only.
#[inline]
pub fn record_fix(
    cache: &mut DiagnosticCache,
    diagnostic: Option<&Diagnostic>,
    signature: Option<&str>,
    patch: String,
    note: Option<String>,
) -> Result<String, AifixError>
{
    match (diagnostic, signature) {
        | (Some(value), _) => cache.record_fix_for_diagnostic(value, patch, note),
        | (None, Some(value)) => cache.record_fix_for_signature(value, patch, note),
        | (None, None) => Err(AifixError::invalid_argument(
            "diagnostic or signature is required to record a fix",
        )),
    }
}

/// Find cached fixes and increment their use counts.
///
/// # Contract
/// Preconditions: diagnostics are normalized. Postconditions: returned matches
/// preserve diagnostic order and each matched fix use count is incremented.
/// Failure modes: none. Panics: none.
#[must_use]
#[inline]
pub fn find_cached_fixes(
    cache: &mut DiagnosticCache,
    diagnostics: &[Diagnostic],
) -> Vec<CachedFixMatch>
{
    cache.take_cached_fixes_for_diagnostics(diagnostics)
}

/// Parse diagnostics from raw input for cache-oriented MCP callers.
///
/// # Contract
/// Preconditions: `input` is UTF-8 diagnostic output for `protocol`.
/// Postconditions: returns normalized diagnostics without invoking tools.
/// Failure modes: adapter parser errors are returned unchanged. Panics: none.
///
/// # Errors
/// Returns parser errors from the selected diagnostic adapter.
#[inline]
pub fn diagnostics_from_input(
    input: &str,
    protocol: Protocol,
) -> Result<Vec<Diagnostic>, AifixError>
{
    parse_diagnostics(protocol, input)
}

/// Extract diagnostics from an optional digest or diagnostic array.
///
/// # Contract
/// Preconditions: at least one argument is `Some`. Postconditions: diagnostics
/// from `diagnostics` take precedence over `digest` and are cloned for callers
/// that need owned replay inputs. Failure modes: both absent returns
/// [`AifixError::InvalidArgument`]. Panics: none.
///
/// # Errors
/// Returns [`AifixError::InvalidArgument`] when both diagnostic sources are
/// absent.
#[inline]
pub fn diagnostics_from_parts(
    diagnostics: Option<&[Diagnostic]>,
    digest: Option<&Digest>,
) -> Result<Vec<Diagnostic>, AifixError>
{
    if let Some(values) = diagnostics {
        return Ok(values.to_owned());
    }
    if let Some(value) = digest {
        return Ok(value.diagnostics.clone());
    }
    Err(AifixError::invalid_argument(
        "diagnostics, digest, or input is required",
    ))
}

/// Load a cache, filter unseen diagnostics, and save changes.
///
/// # Contract
/// Preconditions: diagnostics are normalized and project root is valid when
/// supplied. Postconditions: newly emitted signatures are persisted; returned
/// diagnostics were not previously seen. Failure modes: cache load/save errors
/// return typed errors. Panics: none.
///
/// # Errors
/// Returns cache load or save errors.
#[inline]
pub fn filter_unseen_and_save(
    project_root: Option<&Utf8Path>,
    diagnostics: &[Diagnostic],
) -> Result<Vec<Diagnostic>, AifixError>
{
    let mut cache = load_cache(project_root)?;
    let unseen = cache.filter_unseen_diagnostics(diagnostics);
    save_cache(project_root, &cache)?;
    Ok(unseen)
}

/// Load a cache, record metrics, and save changes.
///
/// # Contract
/// Preconditions: diagnostics are normalized and project root is valid when
/// supplied. Postconditions: metrics are persisted once per supplied
/// diagnostic. Failure modes: cache load/save errors return typed errors.
/// Panics: none.
///
/// # Errors
/// Returns cache load or save errors.
#[inline]
pub fn record_metrics_and_save(
    project_root: Option<&Utf8Path>,
    diagnostics: &[Diagnostic],
) -> Result<(), AifixError>
{
    let mut cache = load_cache(project_root)?;
    cache.record_metrics(diagnostics);
    save_cache(project_root, &cache)
}

/// Record a fix by diagnostic or prevalidated signature and save the cache.
///
/// # Contract
/// Preconditions: either `diagnostic` or `signature` is supplied and `patch` is
/// non-empty. Postconditions: the cached fix is persisted and the canonical
/// signature is returned. Failure modes: invalid arguments, IO, JSON, or schema
/// failures return typed errors. Panics: none.
///
/// # Errors
/// Returns cache load/save errors, malformed signature errors, missing input
/// errors, or empty-patch errors.
#[inline]
pub fn record_fix_and_save(
    project_root: Option<&Utf8Path>,
    diagnostic: Option<&Diagnostic>,
    signature: Option<&str>,
    patch: String,
    note: Option<String>,
) -> Result<String, AifixError>
{
    let mut cache = load_cache(project_root)?;
    let recorded = match (diagnostic, signature) {
        | (Some(value), _) => cache.record_fix_for_diagnostic(value, patch, note)?,
        | (None, Some(value)) => cache.record_fix_for_signature(value, patch, note)?,
        | (None, None) => {
            return Err(AifixError::invalid_argument(
                "diagnostic or signature is required to record a fix",
            ));
        },
    };
    save_cache(project_root, &cache)?;
    Ok(recorded)
}

/// Replay cached fixes for diagnostics according to `mode`.
///
/// # Contract
/// Preconditions: diagnostics are normalized and cached patches are git-apply
/// compatible for the requested project. Postconditions: `Suggest` only returns
/// patch text; `DryRun` verifies every match; `Apply` verifies then applies
/// each match in order. Failure modes: process setup, nonzero git exit status,
/// or cache lookup IO errors return typed errors. Panics: none.
///
/// # Errors
/// Returns current-directory, cache lookup, process spawn, process IO, or
/// nonzero `git apply` errors.
#[inline]
pub fn replay_cached_fixes(
    project_root: Option<&Utf8Path>,
    cache: &mut DiagnosticCache,
    diagnostics: &[Diagnostic],
    mode: ReplayMode,
) -> Result<ReplayResult, AifixError>
{
    let root = match project_root {
        | Some(path) => path.to_path_buf(),
        | None => current_utf8_dir()?,
    };
    let matches = cache.take_cached_fixes_for_diagnostics(diagnostics);
    let mut checked = 0_usize;
    let mut applied = 0_usize;

    match mode {
        | ReplayMode::Suggest => {},
        | ReplayMode::DryRun => {
            for item in &matches {
                git_apply(&root, &["apply", "--check"], item.fix.patch.as_str())?;
                checked = checked.saturating_add(1);
            }
        },
        | ReplayMode::Apply => {
            for item in &matches {
                git_apply(&root, &["apply", "--check"], item.fix.patch.as_str())?;
                checked = checked.saturating_add(1);
                git_apply(&root, &["apply"], item.fix.patch.as_str())?;
                applied = applied.saturating_add(1);
            }
        },
    }

    Ok(ReplayResult {
        mode,
        matches,
        checked,
        applied,
    })
}

/// Load a cache, replay cached fixes, and save use-count changes.
///
/// # Contract
/// Preconditions: diagnostics are normalized and project root is valid when
/// supplied. Postconditions: matched fix use counts and any successful replay
/// side effects are persisted. Failure modes: cache or git failures return
/// typed errors. Panics: none.
///
/// # Errors
/// Returns cache load/save errors or replay errors from
/// [`replay_cached_fixes`].
#[inline]
pub fn replay_cached_fixes_and_save(
    project_root: Option<&Utf8Path>,
    diagnostics: &[Diagnostic],
    mode: ReplayMode,
) -> Result<ReplayResult, AifixError>
{
    let mut cache = load_cache(project_root)?;
    let report = replay_cached_fixes(project_root, &mut cache, diagnostics, mode)?;
    save_cache(project_root, &cache)?;
    Ok(report)
}

/// Record metrics and render deterministic Markdown guidance.
///
/// # Contract
/// Preconditions: diagnostics are normalized. Postconditions: metrics are
/// persisted and Markdown guidance is returned. Failure modes: cache load/save
/// errors return typed errors. Panics: none.
///
/// # Errors
/// Returns cache load or save errors.
#[inline]
pub fn record_guidance_and_save(
    project_root: Option<&Utf8Path>,
    diagnostics: &[Diagnostic],
) -> Result<String, AifixError>
{
    let mut cache = load_cache(project_root)?;
    cache.record_metrics(diagnostics);
    let guidance = cache.render_guidance_markdown();
    save_cache(project_root, &cache)?;
    Ok(guidance)
}

/// Render deterministic Markdown guidance from a cache.
///
/// # Contract
/// Preconditions: `cache` may have empty or populated metrics. Postconditions:
/// returns stable Markdown ordered by map key. Failure modes: allocation may
/// abort through the global allocator. Panics: none.
#[must_use]
#[inline]
pub fn render_guidance_markdown(cache: &DiagnosticCache) -> String
{
    render_metrics_guidance_markdown(&cache.metrics)
}

/// Render deterministic Markdown guidance from metrics.
///
/// # Contract
/// Preconditions: `metrics` may be empty or populated. Postconditions: returns
/// stable Markdown ordered by map key. Failure modes: allocation may abort
/// through the global allocator. Panics: none.
#[must_use]
#[inline]
pub fn render_metrics_guidance_markdown(metrics: &DiagnosticMetrics) -> String
{
    let mut output = String::from("# Diagnostic shape guidance\n\n");
    output.push_str(format!("Total recorded diagnostics: {}\n\n", metrics.total).as_str());
    append_metric_section(&mut output, "By source", &metrics.by_source);
    append_metric_section(&mut output, "By severity", &metrics.by_severity);
    append_metric_section(&mut output, "By code", &metrics.by_code);
    append_metric_section(&mut output, "By signature", &metrics.by_signature);
    if metrics.total == 0 {
        output.push_str("No diagnostic metrics recorded yet.\n");
    }
    output
}

/// Append one Markdown metric section.
///
/// # Contract
/// Preconditions: `output` is writable and `metrics` is ordered.
/// Postconditions: section text is appended in deterministic key order. Failure
/// modes: allocator abort only. Panics: none.
fn append_metric_section(
    output: &mut String,
    title: &str,
    metrics: &BTreeMap<String, usize>,
)
{
    output.push_str(format!("## {title}\n").as_str());
    if metrics.is_empty() {
        output.push_str("- None\n\n");
        return;
    }
    for (key, count) in metrics {
        output.push_str(format!("- `{key}`: {count}\n").as_str());
    }
    output.push('\n');
}

/// Convert a severity into the cache metric key.
///
/// # Contract
/// Preconditions: `severity` is any supported model severity. Postconditions:
/// returns the stable lowercase severity spelling. Failure modes: allocation
/// may abort through the global allocator. Panics: none.
#[must_use]
fn severity_key(severity: Severity) -> String
{
    severity.as_str().to_owned()
}

/// Convert an optional code into the cache metric key.
///
/// # Contract
/// Preconditions: `code` came from a normalized diagnostic. Postconditions:
/// empty and absent codes map to `<none>`. Failure modes: allocation may abort
/// through the global allocator. Panics: none.
#[must_use]
fn code_key(code: Option<&str>) -> String
{
    match code.filter(|value| !value.is_empty()) {
        | Some(value) => value.to_owned(),
        | None => "<none>".to_owned(),
    }
}

/// Return the current directory as a UTF-8 path.
///
/// # Contract
/// Preconditions: the process has a current directory. Postconditions: returns
/// a [`Utf8PathBuf`] for UTF-8 directories. Failure modes: IO or non-UTF-8
/// paths return typed errors. Panics: none.
///
/// # Errors
/// Returns [`AifixError::Io`] when the current directory cannot be read and
/// [`AifixError::Utf8`] when that directory is not valid UTF-8.
fn current_utf8_dir() -> Result<Utf8PathBuf, AifixError>
{
    let path = std::env::current_dir().map_err(AifixError::io)?;
    Utf8PathBuf::from_path_buf(path).map_err(|path| {
        AifixError::utf8(format!(
            "current directory is not valid UTF-8: {}",
            path.display()
        ))
    })
}

/// Run `git` with patch text on standard input.
///
/// # Contract
/// Preconditions: `args` are direct git argv entries and `patch` is non-empty
/// patch text. Postconditions: returns `Ok(())` only for successful git exit
/// status. Failure modes: process spawn, stdin write, wait, or nonzero exit
/// statuses return [`AifixError::Process`]. Panics: none.
///
/// # Errors
/// Returns [`AifixError::InvalidArgument`] for an empty patch and
/// [`AifixError::Process`] when `git` cannot be spawned, written to, waited on,
/// or exits unsuccessfully.
fn git_apply(
    cwd: &Utf8Path,
    args: &[&str],
    patch: &str,
) -> Result<(), AifixError>
{
    if patch.trim().is_empty() {
        return Err(AifixError::invalid_argument(
            "cached fix patch must not be empty",
        ));
    }

    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd.as_std_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AifixError::process(format!("failed to spawn git: {source}")))?;

    let Some(mut stdin) = child.stdin.take()
    else {
        return Err(AifixError::process("failed to open git stdin"));
    };
    io::Write::write_all(&mut stdin, patch.as_bytes()).map_err(|source| {
        AifixError::process(format!("failed to write patch to git stdin: {source}"))
    })?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|source| AifixError::process(format!("failed to wait for git: {source}")))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(output.stderr.as_slice());
    Err(AifixError::process(format!(
        "git {} failed with status {}: {}",
        args.join(" "),
        output.status,
        stderr.trim()
    )))
}
