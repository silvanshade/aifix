//! Project-local diagnostic cache persistence and fix replay.
//!
//! The cache lives at `<projectRoot>/.aifix/diagnostics.json` and stores stable
//! diagnostic signatures, cached patch text, and shape metrics in deterministic
//! JSON.  It is intentionally independent from the MCP transport so CLI, tests,
//! and future embedding callers can share the same behavior.

use alloc::borrow::ToOwned as _;
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
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
use crate::model::Span;
use crate::signature::DiagnosticSignature;
use crate::syntax::SyntaxContextEvidence;
use crate::syntax::SyntaxContextResult;
use crate::syntax::syntax_context_for_diagnostic;

/// Cache schema version written to disk.
///
/// # Contract
/// Preconditions: callers compare this with
/// [`DiagnosticCache::schema_version`]. Postconditions: persisted cache files
/// use this exact version. Failure modes: none. Panics: none.
pub const CACHE_SCHEMA_VERSION: u8 = 2;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Schema-v2 match metadata keyed by exact diagnostic signature.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub match_index: BTreeMap<String, MatchIndexEntry>,
    /// Schema-v2 fix-family metadata keyed by normalized diagnostic family.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fix_families: BTreeMap<String, FixFamilyRecord>,
    /// Aggregated diagnostic-shape metrics.
    #[serde(default)]
    pub metrics: DiagnosticMetrics,
}

impl Default for DiagnosticCache
{
    /// Construct an empty current-schema cache.
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
            match_index: BTreeMap::new(),
            fix_families: BTreeMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for DiagnosticCache
{
    /// Deserialize schema-v1 or schema-v2 cache JSON into a current cache.
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DiagnosticCacheWire
        {
            schema_version: u8,
            #[serde(default)]
            seen: BTreeMap<String, SeenDiagnostic>,
            #[serde(default)]
            fixes: BTreeMap<String, CachedFix>,
            #[serde(default)]
            match_index: BTreeMap<String, MatchIndexEntry>,
            #[serde(default)]
            fix_families: BTreeMap<String, FixFamilyRecord>,
            #[serde(default)]
            metrics: DiagnosticMetrics,
        }

        let wire = DiagnosticCacheWire::deserialize(deserializer)?;
        let mut cache = Self {
            schema_version: wire.schema_version,
            seen: wire.seen,
            fixes: wire.fixes,
            match_index: wire.match_index,
            fix_families: wire.fix_families,
            metrics: wire.metrics,
        };
        if matches!(cache.schema_version, 1 | CACHE_SCHEMA_VERSION) {
            cache = cache
                .migrate_to_current()
                .map_err(serde::de::Error::custom)?;
        }
        Ok(cache)
    }
}

impl DiagnosticCache
{
    /// Validate this cache's schema version.
    ///
    /// # Contract
    /// Preconditions: `self` was loaded or built by a caller. Postconditions:
    /// returns `Ok(())` only for the current schema version. Failure modes:
    /// unsupported versions return [`AifixError::Config`]. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::Config`] when the cache schema version is not
    /// supported by this implementation.
    #[inline]
    pub fn validate(&self) -> Result<(), AifixError>
    {
        if self.schema_version == CACHE_SCHEMA_VERSION {
            Ok(())
        }
        else {
            Err(AifixError::config(format!(
                "unsupported diagnostics cache schema version {}",
                self.schema_version
            )))
        }
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
            Some(diagnostic),
            None,
            Some("syntax-context-unavailable-no-project-root".to_owned()),
        )?;
        Ok(signature)
    }

    /// Store a cached fix for one diagnostic with project syntax context.
    ///
    /// # Contract
    /// Preconditions: `project_root` is the UTF-8 project root that owns the
    /// diagnostic source file. Postconditions: Rust diagnostics enrich
    /// schema-v2 metadata with bounded syntax fingerprints when available.
    /// Failure modes: source read and empty-patch errors are returned.
    /// Panics: none.
    ///
    /// # Errors
    /// Returns source read errors from syntax extraction or invalid patch
    /// errors.
    #[inline]
    pub fn record_fix_for_diagnostic_with_project_root(
        &mut self,
        project_root: &Utf8Path,
        diagnostic: &Diagnostic,
        patch: String,
        note: Option<String>,
    ) -> Result<String, AifixError>
    {
        let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
        let syntax = syntax_context_for_diagnostic(project_root, diagnostic)?;
        let (evidence, audit_reason) = syntax_parts(syntax);
        self.record_fix_entry(
            signature.clone(),
            patch,
            note,
            Some(diagnostic),
            evidence.as_ref(),
            audit_reason,
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
        self.record_fix_entry(
            canonical.clone(),
            patch,
            note,
            None,
            None,
            Some("signature-only-exact-match".to_owned()),
        )?;
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
        for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            if let Some(fix) = self.fixes.get(signature.as_str()) {
                matches.push(CachedFixMatch {
                    diagnostic_index: Some(diagnostic_index),
                    signature,
                    fix: fix.clone(),
                    confidence: MatchConfidence::Exact,
                    audit_reason: None,
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
        for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            if let Some(fix) = self.fixes.get_mut(signature.as_str()) {
                fix.use_count = fix.use_count.saturating_add(1);
                matches.push(CachedFixMatch {
                    diagnostic_index: Some(diagnostic_index),
                    signature,
                    fix: fix.clone(),
                    confidence: MatchConfidence::Exact,
                    audit_reason: None,
                });
            }
        }
        matches
    }

    /// Find cached fixes with syntax-aware fallback and increment use counts.
    ///
    /// # Contract
    /// Preconditions: `project_root` owns diagnostic source paths and
    /// diagnostics are normalized. Postconditions: exact signatures win; when
    /// exact misses, same-node and nearby family matches may be returned with
    /// approximate confidence. Failure modes: supported source read errors are
    /// returned. Panics: none.
    ///
    /// # Errors
    /// Returns syntax source read errors for supported Rust paths.
    #[inline]
    pub fn take_cached_fixes_for_diagnostics_with_project_root(
        &mut self,
        project_root: &Utf8Path,
        diagnostics: &[Diagnostic],
    ) -> Result<Vec<CachedFixMatch>, AifixError>
    {
        let mut matches = Vec::new();
        for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
            let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
            if let Some(fix) = self.fixes.get_mut(signature.as_str()) {
                fix.use_count = fix.use_count.saturating_add(1);
                matches.push(CachedFixMatch {
                    diagnostic_index: Some(diagnostic_index),
                    signature,
                    fix: fix.clone(),
                    confidence: MatchConfidence::Exact,
                    audit_reason: None,
                });
                continue;
            }
            let syntax = syntax_context_for_diagnostic(project_root, diagnostic)?;
            let Some(evidence) = syntax.evidence()
            else {
                continue;
            };
            let family = NormalizedDiagnosticFamily::from_diagnostic(diagnostic);
            let family_key = family_key(&family);
            let Some(family_record) = self.fix_families.get(family_key.as_str())
            else {
                continue;
            };
            let current = SyntaxContextFingerprint::from_evidence(evidence);
            let candidate = family_record
                .signatures
                .iter()
                .filter_map(|candidate_signature| {
                    let entry = self.match_index.get(candidate_signature.as_str())?;
                    let confidence = syntax_match_confidence(&current, &entry.syntax)?;
                    Some((
                        candidate_signature.clone(),
                        confidence,
                        entry.audit_reason.clone(),
                    ))
                })
                .min_by_key(|candidate| (candidate.1, candidate.0.clone()));
            let Some((candidate_signature, confidence, audit_reason)) = candidate
            else {
                continue;
            };
            if let Some(fix) = self.fixes.get_mut(candidate_signature.as_str()) {
                fix.use_count = fix.use_count.saturating_add(1);
                matches.push(CachedFixMatch {
                    diagnostic_index: Some(diagnostic_index),
                    signature: candidate_signature,
                    fix: fix.clone(),
                    confidence,
                    audit_reason,
                });
            }
        }
        Ok(matches)
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
    /// Preconditions: `signature` is canonical and `diagnostic` matches it when
    /// present. Postconditions: the fix map contains the new patch entry and
    /// schema-v2 metadata is refreshed. Failure modes: empty or whitespace-only
    /// patches return typed errors. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when the patch text is empty or
    /// whitespace only.
    fn record_fix_entry(
        &mut self,
        signature: String,
        patch: String,
        note: Option<String>,
        diagnostic: Option<&Diagnostic>,
        syntax: Option<&SyntaxContextEvidence>,
        audit_reason: Option<String>,
    ) -> Result<(), AifixError>
    {
        if patch.trim().is_empty() {
            return Err(AifixError::invalid_argument(
                "cached fix patch must not be empty",
            ));
        }
        let summary = diagnostic.map(DiagnosticSummary::from_diagnostic);
        self.upsert_v2_metadata(
            signature.as_str(),
            patch.as_str(),
            diagnostic,
            summary.as_ref(),
            syntax,
            audit_reason,
        );
        self.fixes.insert(signature, CachedFix {
            patch,
            note,
            summary,
            use_count: 0,
        });
        Ok(())
    }

    /// Upsert schema-v2 metadata for a cached fix.
    ///
    /// # Contract
    /// Preconditions: `signature` is canonical and `patch` is non-empty.
    /// Postconditions: `match_index` has an exact entry; diagnostic-backed
    /// fixes also update `fix_families`. Failure modes: allocator abort only.
    /// Panics: none.
    fn upsert_v2_metadata(
        &mut self,
        signature: &str,
        patch: &str,
        diagnostic: Option<&Diagnostic>,
        summary: Option<&DiagnosticSummary>,
        syntax: Option<&SyntaxContextEvidence>,
        fallback_reason: Option<String>,
    )
    {
        let patch_fingerprint = PatchFingerprint::from_patch(patch);
        let (family_key, spans, audit_reason) = if let Some(diagnostic) = diagnostic {
            let family = NormalizedDiagnosticFamily::from_diagnostic(diagnostic);
            let family_key = family_key(&family);
            let family_record = self
                .fix_families
                .entry(family_key.clone())
                .or_insert_with(|| FixFamilyRecord {
                    key: family_key.clone(),
                    family,
                    signatures: BTreeSet::new(),
                    audit_reasons: BTreeSet::new(),
                });
            family_record.signatures.insert(signature.to_owned());
            if let Some(reason) = fallback_reason.as_ref() {
                family_record.audit_reasons.insert(reason.clone());
            }
            (
                Some(family_key),
                diagnostic
                    .spans
                    .iter()
                    .map(SourceSpanIdentity::from_span)
                    .collect(),
                fallback_reason,
            )
        }
        else if let Some(summary) = summary {
            let family = NormalizedDiagnosticFamily::from_summary(summary);
            let family_key = family_key(&family);
            let family_record = self
                .fix_families
                .entry(family_key.clone())
                .or_insert_with(|| FixFamilyRecord {
                    key: family_key.clone(),
                    family,
                    signatures: BTreeSet::new(),
                    audit_reasons: BTreeSet::new(),
                });
            family_record.signatures.insert(signature.to_owned());
            family_record
                .audit_reasons
                .insert("schema-v1-summary-backfill".to_owned());
            (
                Some(family_key),
                BTreeSet::new(),
                fallback_reason.or_else(|| Some("schema-v1-summary-backfill".to_owned())),
            )
        }
        else {
            (
                None,
                BTreeSet::new(),
                fallback_reason.or_else(|| Some("signature-only-exact-match".to_owned())),
            )
        };

        self.match_index
            .insert(signature.to_owned(), MatchIndexEntry {
                signature: signature.to_owned(),
                confidence: if syntax.is_some() {
                    MatchConfidence::SameNode
                }
                else {
                    MatchConfidence::Exact
                },
                family_key,
                spans,
                syntax: syntax
                    .map(SyntaxContextFingerprint::from_evidence)
                    .unwrap_or_default(),
                patch: patch_fingerprint,
                audit_reason,
            });
    }

    /// Migrate supported older cache schemas into the current schema.
    ///
    /// # Contract
    /// Preconditions: `self` was deserialized from cache JSON. Postconditions:
    /// schema-v1 caches are promoted to schema-v2 with exact-only match-index
    /// backfill; current caches are unchanged. Failure modes: unsupported
    /// versions return typed config errors. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::Config`] for unsupported schema versions.
    fn migrate_to_current(mut self) -> Result<Self, AifixError>
    {
        match self.schema_version {
            | CACHE_SCHEMA_VERSION => {
                self.backfill_missing_v2_metadata();
                Ok(self)
            },
            | 1 => {
                self.schema_version = CACHE_SCHEMA_VERSION;
                self.backfill_missing_v2_metadata();
                Ok(self)
            },
            | other => Err(AifixError::config(format!(
                "unsupported diagnostics cache schema version {other}"
            ))),
        }
    }

    /// Backfill exact-only schema-v2 metadata for cached fixes that lack it.
    ///
    /// # Contract
    /// Preconditions: `fixes` is the authoritative exact replay map.
    /// Postconditions: every cached fix has a match-index entry; fixes with
    /// summaries also have family records. Failure modes: allocator abort only.
    /// Panics: none.
    fn backfill_missing_v2_metadata(&mut self)
    {
        let fixes: Vec<(String, String, Option<DiagnosticSummary>)> = self
            .fixes
            .iter()
            .filter(|entry| !self.match_index.contains_key(entry.0.as_str()))
            .map(|(signature, fix)| (signature.clone(), fix.patch.clone(), fix.summary.clone()))
            .collect();
        for (signature, patch, summary) in fixes {
            self.upsert_v2_metadata(
                signature.as_str(),
                patch.as_str(),
                None,
                summary.as_ref(),
                None,
                Some("schema-v1-summary-backfill".to_owned()),
            );
        }
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
    /// Diagnostic summary captured when recording a diagnostic-backed fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<DiagnosticSummary>,
    /// Number of replay lookups that selected this fix.
    #[serde(default, skip_serializing_if = "is_default")]
    pub use_count: usize,
}

/// Normalized diagnostic family used by schema-v2 cache metadata.
///
/// # Contract
/// Preconditions: values are derived from normalized diagnostics or v1
/// summaries. Postconditions: ordering is deterministic and the family omits
/// volatile spans. Failure modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NormalizedDiagnosticFamily
{
    /// Diagnostic source tool or protocol family.
    pub source: String,
    /// Structured diagnostic code when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Normalized severity spelling.
    pub severity: String,
    /// Stable message-family fingerprint.
    pub message_family: String,
}

/// Stable source span identity stored in schema-v2 match metadata.
///
/// # Contract
/// Preconditions: values are copied from normalized spans. Postconditions:
/// serde omits absent coordinates. Failure modes: none while held as a value.
/// Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceSpanIdentity
{
    /// File path or URI reported by the diagnostic source.
    pub file: String,
    /// One-based start line when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// One-based start column when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// One-based end line when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// One-based end column when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
}

/// Syntax-context fingerprint slots reserved for conservative matching.
///
/// # Contract
/// Preconditions: slots are optional because parser context may be absent.
/// Postconditions: empty fingerprints serialize as `{}`. Failure modes: none
/// while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxContextFingerprint
{
    /// Fingerprint for the diagnostic's exact syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Fingerprint for the parent syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Fingerprints for nearby sibling syntax nodes.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub nearby: BTreeSet<String>,
    /// Byte start for the selected syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_start: Option<usize>,
    /// Byte end for the selected syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_end: Option<usize>,
    /// One-based start line for the selected syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    /// One-based end line for the selected syntax node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// Stable line-ending spelling observed in source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<String>,
    /// Stable leading-whitespace signal for the selected line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_whitespace: Option<String>,
}

/// Match confidence recorded for audit and future suggestion ranking.
///
/// # Contract
/// Preconditions: values describe how a cached fix matched a diagnostic.
/// Postconditions: `Exact` remains the only unattended replay path. Failure
/// modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchConfidence
{
    /// Exact diagnostic signature match.
    Exact,
    /// Same syntax node match, reserved for dry-run/suggestion metadata.
    SameNode,
    /// Nearby syntax context match, reserved for dry-run/suggestion metadata.
    Nearby,
    /// No syntax-aware match was available.
    NoMatch,
}

/// Fingerprint metadata for a cached patch.
///
/// # Contract
/// Preconditions: values are derived from patch text. Postconditions:
/// fingerprint fields are deterministic for the same patch bytes. Failure
/// modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchFingerprint
{
    /// Stable hash of the patch text.
    pub digest: String,
    /// Number of UTF-8 bytes in the patch.
    pub byte_len: usize,
    /// Number of lines in the patch text.
    pub line_count: usize,
}

/// Schema-v2 match-index record keyed by exact diagnostic signature.
///
/// # Contract
/// Preconditions: signature keys are canonical diagnostic signatures.
/// Postconditions: exact replay callers continue to use `fixes`; this metadata
/// is audit/index data only. Failure modes: none while held as a value. Panics:
/// none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchIndexEntry
{
    /// Canonical diagnostic signature that owns the cached fix.
    pub signature: String,
    /// Confidence represented by this index entry.
    pub confidence: MatchConfidence,
    /// Family key for diagnostics recorded with semantic summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_key: Option<String>,
    /// Source spans that identify the original diagnostic location.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub spans: BTreeSet<SourceSpanIdentity>,
    /// Optional syntax context slots for future approximate matching.
    #[serde(default, skip_serializing_if = "SyntaxContextFingerprint::is_empty")]
    pub syntax: SyntaxContextFingerprint,
    /// Patch fingerprint for audit and duplicate detection.
    pub patch: PatchFingerprint,
    /// Audit reason for exact-only or degraded entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_reason: Option<String>,
}

/// Schema-v2 fix family grouping signatures with the same diagnostic family.
///
/// # Contract
/// Preconditions: `key` is derived from `family`. Postconditions: signatures
/// are stored in deterministic order. Failure modes: none while held as a
/// value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixFamilyRecord
{
    /// Deterministic family key.
    pub key: String,
    /// Normalized family identity.
    pub family: NormalizedDiagnosticFamily,
    /// Exact signatures that belong to this family.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub signatures: BTreeSet<String>,
    /// Audit or fallback reasons observed while building the family.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub audit_reasons: BTreeSet<String>,
}

impl NormalizedDiagnosticFamily
{
    /// Build a family from one normalized diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` is normalized. Postconditions: volatile
    /// spans are excluded and message text is reduced to a stable family
    /// fingerprint. Failure modes: allocator abort only. Panics: none.
    #[must_use]
    #[inline]
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        Self {
            source: diagnostic.source.clone(),
            code: diagnostic.code.clone(),
            severity: diagnostic.severity.as_str().to_owned(),
            message_family: message_family_key(diagnostic.message.as_str()),
        }
    }

    /// Build a family from compact diagnostic summary metadata.
    ///
    /// # Contract
    /// Preconditions: `summary` came from a normalized diagnostic.
    /// Postconditions: the same family fields are derived without span
    /// context. Failure modes: allocator abort only. Panics: none.
    #[must_use]
    #[inline]
    pub fn from_summary(summary: &DiagnosticSummary) -> Self
    {
        Self {
            source: summary.source.clone(),
            code: summary.code.clone(),
            severity: summary.severity.clone(),
            message_family: message_family_key(summary.message.as_str()),
        }
    }
}

impl SourceSpanIdentity
{
    /// Build a source span identity from a normalized span.
    ///
    /// # Contract
    /// Preconditions: `span` is normalized. Postconditions: all reported span
    /// coordinates are copied unchanged. Failure modes: allocator abort only.
    /// Panics: none.
    #[must_use]
    #[inline]
    pub fn from_span(span: &Span) -> Self
    {
        Self {
            file: span.file.clone(),
            line: span.line,
            column: span.column,
            end_line: span.end_line,
            end_column: span.end_column,
        }
    }
}

impl SyntaxContextFingerprint
{
    /// Return whether all syntax-context slots are empty.
    ///
    /// # Contract
    /// Preconditions: none. Postconditions: returns true only when no syntax
    /// slots are populated. Failure modes: none. Panics: none.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool
    {
        self.node.is_none()
            && self.parent.is_none()
            && self.nearby.is_empty()
            && self.byte_start.is_none()
            && self.byte_end.is_none()
            && self.line_start.is_none()
            && self.line_end.is_none()
            && self.line_ending.is_none()
            && self.leading_whitespace.is_none()
    }

    /// Build cache syntax fingerprints from bounded syntax evidence.
    ///
    /// # Contract
    /// Preconditions: `evidence` was produced by the syntax module.
    /// Postconditions: only stable fingerprints and bounded positional signals
    /// are copied into cache metadata. Failure modes: allocator abort only.
    /// Panics: none.
    #[must_use]
    #[inline]
    pub fn from_evidence(evidence: &SyntaxContextEvidence) -> Self
    {
        Self {
            node: Some(evidence.node.clone()),
            parent: evidence.parent.clone(),
            nearby: evidence.nearby.iter().cloned().collect(),
            byte_start: Some(evidence.node_byte_start),
            byte_end: Some(evidence.node_byte_end),
            line_start: Some(evidence.node_line_start),
            line_end: Some(evidence.node_line_end),
            line_ending: Some(
                match evidence.line_ending {
                    | crate::syntax::LineEndingKind::None => "none",
                    | crate::syntax::LineEndingKind::Lf => "lf",
                    | crate::syntax::LineEndingKind::Crlf => "crlf",
                    | crate::syntax::LineEndingKind::Mixed => "mixed",
                }
                .to_owned(),
            ),
            leading_whitespace: Some(format!(
                "spaces:{};tabs:{};hash:{}",
                evidence.leading_whitespace.spaces,
                evidence.leading_whitespace.tabs,
                evidence.leading_whitespace.fingerprint
            )),
        }
    }
}

impl PatchFingerprint
{
    /// Build deterministic patch fingerprint metadata.
    ///
    /// # Contract
    /// Preconditions: `patch` is the persisted patch text. Postconditions:
    /// digest, byte length, and line count describe exactly that text. Failure
    /// modes: allocator abort only. Panics: none.
    #[must_use]
    #[inline]
    pub fn from_patch(patch: &str) -> Self
    {
        Self {
            digest: stable_hash_hex(patch.as_bytes()),
            byte_len: patch.len(),
            line_count: patch.lines().count(),
        }
    }
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
/// signature and fix data are cloned for stable reporting, with confidence kept
/// for audit-aware replay behavior. Failure modes: none while held as a value.
/// Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFixMatch
{
    /// Zero-based input diagnostic index when this match came from replay
    /// lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_index: Option<usize>,
    /// Signature that selected this cached fix.
    pub signature: String,
    /// Cached fix selected for the signature.
    pub fix: CachedFix,
    /// Confidence for this cached fix candidate.
    pub confidence: MatchConfidence,
    /// Audit reason attached to degraded or exact-only metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_reason: Option<String>,
}

/// Per-diagnostic replay audit report.
///
/// # Contract
/// Preconditions: constructed in diagnostic input order. Postconditions: every
/// input diagnostic has exactly one audit entry, including no-match entries.
/// Failure modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayDiagnosticAudit
{
    /// Zero-based index in the caller-supplied diagnostic list.
    pub diagnostic_index: usize,
    /// Confidence for the best candidate, or `NoMatch`.
    pub confidence: MatchConfidence,
    /// Cached signature selected for replay when any candidate matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_signature: Option<String>,
    /// Deterministic normalized diagnostic family key when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_key: Option<String>,
    /// Positive syntax or exact-signature evidence for the match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax_evidence: Option<String>,
    /// Fallback or degradation reason when no trusted syntax match exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Whether `git apply --check` was run for this diagnostic candidate.
    pub git_check_ran: bool,
    /// Whether `git apply` was run for this diagnostic candidate.
    pub apply_ran: bool,
    /// Whether apply mode skipped this approximate candidate by policy.
    pub skipped_approximate_apply: bool,
}

/// Result of a cached fix replay operation.
///
/// # Contract
/// Preconditions: constructed by [`replay_cached_fixes`]. Postconditions:
/// `matches` preserves existing exact-match patch reporting, `diagnostics`
/// preserves diagnostic input order for audit reporting, `checked` counts
/// successful `git apply --check` calls, and `applied` counts successful
/// `git apply` calls. Failure modes: none while held as a value. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayResult
{
    /// Replay mode requested by the caller.
    pub mode: ReplayMode,
    /// Matching cached fixes in diagnostic input order.
    pub matches: Vec<CachedFixMatch>,
    /// Per-diagnostic deterministic audit reports in input order.
    pub diagnostics: Vec<ReplayDiagnosticAudit>,
    /// Number of patches verified by `git apply --check`.
    pub checked: usize,
    /// Number of patches applied by `git apply`.
    pub applied: usize,
    /// Number of approximate candidates skipped in apply mode.
    pub skipped_approximate_applies: usize,
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
/// Postconditions: returns a current-schema cache, empty when the file does
/// not exist, and migrates supported v1 JSON in memory. Failure modes: IO,
/// JSON, or unsupported schema versions return typed errors. Panics: none.
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
            let cache = cache.migrate_to_current()?;
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
/// missing cache files load as empty current-schema caches and supported v1
/// JSON is migrated. Failure modes: IO, JSON, or schema errors return typed
/// errors. Panics: none.
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
    let recorded = match (diagnostic, signature, project_root) {
        | (Some(value), _, Some(root)) => {
            cache.record_fix_for_diagnostic_with_project_root(root, value, patch, note)?
        },
        | (Some(value), _, None) => cache.record_fix_for_diagnostic(value, patch, note)?,
        | (None, Some(value), _) => cache.record_fix_for_signature(value, patch, note)?,
        | (None, None, _) => {
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
/// compatible for the requested project. Postconditions: `Suggest` returns
/// exact and approximate candidates without invoking git; `DryRun` verifies
/// every candidate; `Apply` verifies and applies exact candidates only while
/// auditing approximate skips. Failure modes: process setup, nonzero git exit
/// status, or cache lookup IO errors return typed errors. Panics: none.
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
    let matches = cache.take_cached_fixes_for_diagnostics_with_project_root(&root, diagnostics)?;
    let mut checked = 0_usize;
    let mut applied = 0_usize;
    let mut skipped_approximate_applies = 0_usize;
    let mut replay_audit = replay_audit_for_matches(diagnostics, &matches);

    match mode {
        | ReplayMode::Suggest => {},
        | ReplayMode::DryRun => {
            for item in &matches {
                git_apply(&root, &["apply", "--check"], item.fix.patch.as_str())?;
                checked = checked.saturating_add(1);
                mark_git_check(&mut replay_audit, item);
            }
        },
        | ReplayMode::Apply => {
            for item in &matches {
                if item.confidence != MatchConfidence::Exact {
                    mark_skipped_approximate_apply(&mut replay_audit, item);
                    skipped_approximate_applies = skipped_approximate_applies.saturating_add(1);
                    continue;
                }
                git_apply(&root, &["apply", "--check"], item.fix.patch.as_str())?;
                checked = checked.saturating_add(1);
                mark_git_check(&mut replay_audit, item);
                git_apply(&root, &["apply"], item.fix.patch.as_str())?;
                applied = applied.saturating_add(1);
                mark_apply(&mut replay_audit, item);
            }
        },
    }

    Ok(ReplayResult {
        mode,
        matches,
        diagnostics: replay_audit,
        checked,
        applied,
        skipped_approximate_applies,
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

/// Build a deterministic key for a normalized diagnostic family.
///
/// # Contract
/// Preconditions: `family` fields are normalized. Postconditions: returns a
/// stable JSON-independent key. Failure modes: allocation may abort through the
/// global allocator. Panics: none.
#[must_use]
fn family_key(family: &NormalizedDiagnosticFamily) -> String
{
    let code = family.code.as_deref().unwrap_or("<none>");
    format!(
        "{}|{}|{}|{}",
        family.source, code, family.severity, family.message_family
    )
}

/// Split syntax context into cache evidence and fallback audit reason.
fn syntax_parts(result: SyntaxContextResult) -> (Option<SyntaxContextEvidence>, Option<String>)
{
    match result {
        | SyntaxContextResult::Evidence(evidence) => (Some(evidence), None),
        | SyntaxContextResult::NoMatch { reason } => (None, Some(reason)),
    }
}

/// Return approximate syntax match confidence for two fingerprints.
fn syntax_match_confidence(
    current: &SyntaxContextFingerprint,
    cached: &SyntaxContextFingerprint,
) -> Option<MatchConfidence>
{
    if current.node.is_some() && current.node == cached.node {
        return Some(MatchConfidence::SameNode);
    }
    if current.parent.is_some() && current.parent == cached.parent {
        return Some(MatchConfidence::Nearby);
    }
    if !current.nearby.is_empty()
        && current
            .nearby
            .iter()
            .any(|item| cached.nearby.contains(item))
    {
        return Some(MatchConfidence::Nearby);
    }
    None
}

/// Build replay audit entries for every input diagnostic.
fn replay_audit_for_matches(
    diagnostics: &[Diagnostic],
    matches: &[CachedFixMatch],
) -> Vec<ReplayDiagnosticAudit>
{
    let mut audits = Vec::new();
    let mut match_iter = matches.iter();
    let mut next_match = match_iter.next();
    for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
        let signature = DiagnosticSignature::from_diagnostic(diagnostic).as_key();
        let family = NormalizedDiagnosticFamily::from_diagnostic(diagnostic);
        let family_key = family_key(&family);
        if let Some(item) = next_match
            && item.diagnostic_index == Some(diagnostic_index)
            && (item.signature == signature || item.confidence != MatchConfidence::Exact)
        {
            audits.push(ReplayDiagnosticAudit {
                diagnostic_index,
                confidence: item.confidence,
                matched_signature: Some(item.signature.clone()),
                family_key: Some(family_key),
                syntax_evidence: Some(match item.confidence {
                    | MatchConfidence::Exact => "exact-signature".to_owned(),
                    | MatchConfidence::SameNode => "same-node".to_owned(),
                    | MatchConfidence::Nearby => "nearby-context".to_owned(),
                    | MatchConfidence::NoMatch => "no-match".to_owned(),
                }),
                fallback_reason: item.audit_reason.clone(),
                git_check_ran: false,
                apply_ran: false,
                skipped_approximate_apply: false,
            });
            next_match = match_iter.next();
        }
        else {
            audits.push(ReplayDiagnosticAudit {
                diagnostic_index,
                confidence: MatchConfidence::NoMatch,
                matched_signature: None,
                family_key: Some(family_key),
                syntax_evidence: None,
                fallback_reason: Some("no-cached-fix-candidate".to_owned()),
                git_check_ran: false,
                apply_ran: false,
                skipped_approximate_apply: false,
            });
        }
    }
    audits
}

/// Mark audit entries whose candidate was checked with git.
fn mark_git_check(
    audits: &mut [ReplayDiagnosticAudit],
    item: &CachedFixMatch,
)
{
    for audit in audits {
        if item_matches_audit(item, audit) {
            audit.git_check_ran = true;
            return;
        }
    }
}

/// Mark audit entries whose candidate was applied with git.
fn mark_apply(
    audits: &mut [ReplayDiagnosticAudit],
    item: &CachedFixMatch,
)
{
    for audit in audits {
        if item_matches_audit(item, audit) {
            audit.apply_ran = true;
            return;
        }
    }
}

/// Mark audit entries skipped because approximate matches are not auto-applied.
fn mark_skipped_approximate_apply(
    audits: &mut [ReplayDiagnosticAudit],
    item: &CachedFixMatch,
)
{
    for audit in audits {
        if item_matches_audit(item, audit) {
            audit.skipped_approximate_apply = true;
            audit.fallback_reason = Some("approximate-auto-apply-forbidden".to_owned());
            return;
        }
    }
}

/// Return whether a replay match corresponds to an audit entry.
fn item_matches_audit(
    item: &CachedFixMatch,
    audit: &ReplayDiagnosticAudit,
) -> bool
{
    item.diagnostic_index == Some(audit.diagnostic_index)
        || (item.diagnostic_index.is_none()
            && audit.matched_signature.as_deref() == Some(item.signature.as_str()))
}

/// Build a stable message-family fingerprint from diagnostic text.
///
/// # Contract
/// Preconditions: `message` is normalized diagnostic text. Postconditions:
/// whitespace-only differences collapse to the same key. Failure modes:
/// allocation may abort through the global allocator. Panics: none.
#[must_use]
fn message_family_key(message: &str) -> String
{
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    stable_hash_hex(normalized.as_bytes())
}

/// Compute a deterministic FNV-1a hash encoded as fixed-width hexadecimal.
///
/// # Contract
/// Preconditions: `bytes` is the byte sequence to fingerprint. Postconditions:
/// returns the same digest for the same bytes across platforms. Failure modes:
/// allocation may abort through the global allocator. Panics: none.
#[must_use]
fn stable_hash_hex(bytes: &[u8]) -> String
{
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
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

/// Return whether a value is the default for serde omission.
fn is_default<T>(value: &T) -> bool
where
    T: Default + PartialEq,
{
    value == &T::default()
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

#[cfg(test)]
mod tests
{
    use std::fs;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::model::Suggestion;

    fn temp_project(name: &str) -> Utf8PathBuf
    {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX_EPOCH")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("aifix-cache-{name}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("temporary project directory should be created");
        Utf8PathBuf::from_path_buf(path).expect("temporary project path should be UTF-8")
    }

    fn rust_project(name: &str) -> Utf8PathBuf
    {
        let root = temp_project(name);
        fs::create_dir_all(root.join("src")).expect("src directory should be created");
        fs::write(
            root.join("src/lib.rs"),
            "pub fn demo() {\n    let value = 1;\n    let other = value;\n}\n",
        )
        .expect("Rust source should be written");
        root
    }

    fn diagnostic(
        file: &str,
        line: u32,
        column: u32,
        end_column: u32,
    ) -> Diagnostic
    {
        Diagnostic::new(
            "rustc",
            Some("E0308".to_owned()),
            Severity::Error,
            "mismatched types",
        )
        .with_details(
            vec![Span::new(
                file,
                Some(line),
                Some(column),
                Some(line),
                Some(end_column),
            )],
            vec![Suggestion::new(
                "consider changing this binding",
                Some("replacement".to_owned()),
                Some(Span::new(
                    file,
                    Some(line),
                    Some(column),
                    Some(line),
                    Some(end_column),
                )),
            )],
            None,
        )
    }

    fn invalid_patch() -> String
    {
        "this is intentionally not a git patch\n".to_owned()
    }

    fn exactly_one<'slice, T>(
        slice: &'slice [T],
        context: &str,
    ) -> Result<&'slice T, Box<dyn core::error::Error>>
    {
        let mut items = slice.iter();
        let Some(item) = items.next()
        else {
            return Err(std::io::Error::other(format!("{context}; got 0")).into());
        };
        if items.next().is_some() {
            return Err(std::io::Error::other(format!("{context}; got {}", slice.len())).into());
        }
        Ok(item)
    }

    #[test]
    fn schema_v1_json_deserializes_into_schema_v2_with_exact_only_metadata()
    {
        let diagnostic = diagnostic("src/lib.rs", 2, 9, 14);
        let signature = DiagnosticSignature::from_diagnostic(&diagnostic).as_key();
        let mut fixes = serde_json::Map::new();
        fixes.insert(
            signature.clone(),
            serde_json::json!({
                "patch": "diff --git a/src/lib.rs b/src/lib.rs\n",
                "note": "kept",
                "summary": DiagnosticSummary::from_diagnostic(&diagnostic),
                "use_count": 3_usize
            }),
        );
        let json = serde_json::json!({
            "schema_version": 1_u8,
            "fixes": fixes
        });

        let cache: DiagnosticCache =
            serde_json::from_value(json).expect("schema v1 cache should deserialize");

        assert_eq!(cache.schema_version, CACHE_SCHEMA_VERSION);
        let fix = cache
            .fixes
            .get(&signature)
            .expect("fix should be preserved");
        assert_eq!(fix.note.as_deref(), Some("kept"));
        assert_eq!(fix.use_count, 3_usize);
        let entry = cache
            .match_index
            .get(&signature)
            .expect("v1 fix should gain match-index metadata");
        assert_eq!(entry.confidence, MatchConfidence::Exact);
        assert_eq!(entry.signature, signature);
        assert!(entry.syntax.is_empty());
        let family_key = entry
            .family_key
            .as_ref()
            .expect("summary-backed v1 fix should gain a family key");
        let family = cache
            .fix_families
            .get(family_key)
            .expect("summary-backed v1 fix should gain family metadata");
        assert!(family.signatures.contains(&entry.signature));
        assert!(family.audit_reasons.contains("schema-v1-summary-backfill"));
    }

    #[test]
    fn exact_replay_returns_exact_match_and_exact_audit_entry()
    -> Result<(), Box<dyn core::error::Error>>
    {
        let root = rust_project("exact-replay");
        let diagnostic = diagnostic("src/lib.rs", 2, 9, 14);
        let mut cache = DiagnosticCache::default();
        let signature = cache.record_fix_for_diagnostic_with_project_root(
            &root,
            &diagnostic,
            invalid_patch(),
            Some("cached".to_owned()),
        )?;

        let result = replay_cached_fixes(
            Some(&root),
            &mut cache,
            core::slice::from_ref(&diagnostic),
            ReplayMode::Suggest,
        )?;

        let cached_match = exactly_one(
            result.matches.as_slice(),
            "suggest replay should return one cached fix match",
        )?;
        assert_eq!(cached_match.signature, signature);
        assert_eq!(cached_match.confidence, MatchConfidence::Exact);
        let audit = exactly_one(
            result.diagnostics.as_slice(),
            "suggest replay should return one diagnostic audit entry",
        )?;
        assert_eq!(audit.confidence, MatchConfidence::Exact);
        assert_eq!(audit.syntax_evidence.as_deref(), Some("exact-signature"));
        assert_eq!(audit.matched_signature.as_deref(), Some(signature.as_str()));
        assert!(!audit.git_check_ran);
        Ok(())
    }

    #[test]
    fn same_family_same_node_replay_suggests_same_node_candidate()
    -> Result<(), Box<dyn core::error::Error>>
    {
        let root = rust_project("same-node-suggest");
        let recorded = diagnostic("src/lib.rs", 2, 9, 14);
        let replayed = diagnostic("src/lib.rs", 2, 10, 13);
        assert_ne!(
            DiagnosticSignature::from_diagnostic(&recorded).as_key(),
            DiagnosticSignature::from_diagnostic(&replayed).as_key()
        );
        let mut cache = DiagnosticCache::default();
        let recorded_signature = cache.record_fix_for_diagnostic_with_project_root(
            &root,
            &recorded,
            invalid_patch(),
            None,
        )?;

        let result = replay_cached_fixes(
            Some(&root),
            &mut cache,
            core::slice::from_ref(&replayed),
            ReplayMode::Suggest,
        )?;

        let cached_match = exactly_one(
            result.matches.as_slice(),
            "suggest replay should return one cached fix match",
        )?;
        assert_eq!(cached_match.signature, recorded_signature);
        assert_eq!(cached_match.confidence, MatchConfidence::SameNode);
        let audit = exactly_one(
            result.diagnostics.as_slice(),
            "suggest replay should return one diagnostic audit entry",
        )?;
        assert_eq!(audit.confidence, MatchConfidence::SameNode);
        assert_eq!(audit.syntax_evidence.as_deref(), Some("same-node"));
        assert_eq!(result.checked, 0);
        assert_eq!(result.applied, 0);
        Ok(())
    }

    #[test]
    fn apply_mode_skips_approximate_candidate_without_invoking_git()
    -> Result<(), Box<dyn core::error::Error>>
    {
        let root = rust_project("same-node-apply");
        let recorded = diagnostic("src/lib.rs", 2, 9, 14);
        let replayed = diagnostic("src/lib.rs", 2, 10, 13);
        let mut cache = DiagnosticCache::default();
        cache.record_fix_for_diagnostic_with_project_root(
            &root,
            &recorded,
            invalid_patch(),
            None,
        )?;

        let result = replay_cached_fixes(
            Some(&root),
            &mut cache,
            core::slice::from_ref(&replayed),
            ReplayMode::Apply,
        )?;

        let cached_match = exactly_one(
            result.matches.as_slice(),
            "apply replay should return one cached fix match",
        )?;
        assert_eq!(cached_match.confidence, MatchConfidence::SameNode);
        assert_eq!(result.checked, 0);
        assert_eq!(result.applied, 0);
        assert_eq!(result.skipped_approximate_applies, 1_usize);
        let audit = exactly_one(
            result.diagnostics.as_slice(),
            "apply replay should return one diagnostic audit entry",
        )?;
        assert_eq!(audit.confidence, MatchConfidence::SameNode);
        assert!(audit.skipped_approximate_apply);
        assert!(!audit.git_check_ran);
        assert!(!audit.apply_ran);
        assert_eq!(
            audit.fallback_reason.as_deref(),
            Some("approximate-auto-apply-forbidden")
        );
        Ok(())
    }

    #[test]
    fn unsupported_source_records_explicit_fallback_and_does_not_text_match()
    -> Result<(), Box<dyn core::error::Error>>
    {
        let root = temp_project("unsupported-source");
        fs::write(root.join("note.txt"), "let value = 1;\n")?;
        let recorded = diagnostic("note.txt", 1, 5, 10);
        let replayed = diagnostic("note.txt", 1, 6, 9);
        let mut cache = DiagnosticCache::default();
        let recorded_signature = cache.record_fix_for_diagnostic_with_project_root(
            &root,
            &recorded,
            invalid_patch(),
            None,
        )?;
        let entry = cache
            .match_index
            .get(&recorded_signature)
            .expect("recorded fix should have match-index metadata");
        assert_eq!(
            entry.audit_reason.as_deref(),
            Some("unsupported-source-path")
        );
        let family_key = entry
            .family_key
            .as_ref()
            .expect("diagnostic-backed unsupported source should have a family key");
        assert!(
            cache
                .fix_families
                .get(family_key)
                .expect("family metadata should exist")
                .audit_reasons
                .contains("unsupported-source-path")
        );

        let result = replay_cached_fixes(
            Some(&root),
            &mut cache,
            core::slice::from_ref(&replayed),
            ReplayMode::Suggest,
        )?;

        assert!(result.matches.is_empty());
        let audit = exactly_one(
            result.diagnostics.as_slice(),
            "unsupported source replay should return one diagnostic audit entry",
        )?;
        assert_eq!(audit.confidence, MatchConfidence::NoMatch);
        assert_eq!(
            audit.fallback_reason.as_deref(),
            Some("no-cached-fix-candidate")
        );
        assert_eq!(result.checked, 0);
        assert_eq!(result.applied, 0);
        Ok(())
    }
}
