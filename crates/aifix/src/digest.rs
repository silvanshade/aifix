//! Digest construction for normalized diagnostics.
//!
//! A digest is the stable artifact consumed by renderers and agents.  The
//! builder removes exact semantic duplicates without comparing preserved raw
//! JSON payloads, computes aggregate counts, groups related diagnostics, and
//! preserves the invocation that produced the input.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::explain::explain_code;
use crate::model::Counts;
use crate::model::Diagnostic;
use crate::model::Digest;
use crate::model::Group;
use crate::model::Invocation;
use crate::model::Severity;
use crate::model::Span;
use crate::model::Suggestion;

/// Build an agent-friendly digest from normalized diagnostics and invocation
/// data.
///
/// Exact duplicate diagnostics are removed before counting using a normalized
/// fingerprint built from semantic diagnostic fields only; preserved raw JSON
/// payloads never participate in deduplication. Groups are keyed by diagnostic
/// source plus either code or, when no code exists, message text. Group samples
/// respect `max_diagnostics`; aggregate counts always describe the full
/// deduplicated diagnostic set.
///
/// # Contract
/// Preconditions: diagnostics are already normalized into the crate model.
/// Postconditions: the returned digest contains deduplicated diagnostics,
/// counts whose total equals the deduplicated length, and groups whose samples
/// never exceed their represented count or the requested cap.
/// Failure modes: none.
/// Panics: debug builds may panic if aggregate count or group sample invariants
/// are violated after digest construction.
#[must_use]
#[inline]
pub fn build_digest(
    diagnostics: Vec<Diagnostic>,
    invocation: Invocation,
    max_diagnostics: Option<usize>,
) -> Digest
{
    let mut seen = BTreeMap::<DiagnosticFingerprint, Vec<DiagnosticIdentity>>::new();
    let mut deduped = Vec::with_capacity(diagnostics.len());

    for diagnostic in diagnostics {
        let identity = DiagnosticIdentity::from_diagnostic(&diagnostic);
        let fingerprint = identity.fingerprint();
        let bucket = seen.entry(fingerprint).or_default();
        if bucket.iter().any(|existing| existing == &identity) {
            continue;
        }
        bucket.push(identity);
        deduped.push(diagnostic);
    }

    let counts = count_diagnostics(&deduped);
    let groups = group_diagnostics(&deduped, max_diagnostics);

    debug_assert_eq!(
        counts.total,
        deduped.len(),
        "digest total must match deduplicated diagnostics"
    );
    debug_assert!(
        groups
            .iter()
            .all(|group| group.diagnostics.len() <= group.count),
        "group samples cannot exceed represented counts"
    );

    Digest {
        invocation,
        counts,
        groups,
        diagnostics: deduped,
    }
}

/// Compute aggregate counts by source and severity.
///
/// # Contract
/// Preconditions: diagnostics are normalized and may be empty.
/// Postconditions: returned totals equal `diagnostics.len()` and per-source and
/// per-severity maps account for every diagnostic exactly once.
/// Failure modes: none.
/// Panics: debug builds may panic if per-severity or per-source totals diverge
/// from the diagnostic slice length.
#[must_use]
fn count_diagnostics(diagnostics: &[Diagnostic]) -> Counts
{
    let mut by_severity = BTreeMap::<Severity, usize>::new();
    let mut by_source = BTreeMap::<String, usize>::new();

    for diagnostic in diagnostics {
        *by_severity.entry(diagnostic.severity).or_default() += 1;
        *by_source.entry(diagnostic.source.clone()).or_default() += 1;
    }

    let counts = Counts {
        total: diagnostics.len(),
        by_severity,
        by_source,
    };
    debug_assert_eq!(
        counts.by_severity.values().sum::<usize>(),
        counts.total,
        "severity counts must sum to total diagnostics"
    );
    debug_assert_eq!(
        counts.by_source.values().sum::<usize>(),
        counts.total,
        "source counts must sum to total diagnostics"
    );
    counts
}

/// Group diagnostics by the source and structured code/message fallback.
///
/// # Contract
/// Preconditions: diagnostics are normalized and may be empty;
/// `max_diagnostics` is a per-group sample cap when present.
/// Postconditions: each returned group represents at least one diagnostic,
/// group counts sum to `diagnostics.len()`, and sample lengths are capped by
/// both the represented count and `max_diagnostics`.
/// Failure modes: none.
/// Panics: debug builds may panic if materialized group counts or sample caps
/// do not match the input diagnostics.
#[must_use]
fn group_diagnostics(
    diagnostics: &[Diagnostic],
    max_diagnostics: Option<usize>,
) -> Vec<Group>
{
    let mut groups = BTreeMap::<GroupKey, GroupAccumulator>::new();
    let sample_limit = match max_diagnostics {
        | Some(limit) => limit,
        | None => usize::MAX,
    };

    for diagnostic in diagnostics {
        let key = GroupKey::from_diagnostic(diagnostic);
        let group_code = diagnostic.code.clone().filter(|value| !value.is_empty());
        let entry = groups.entry(key).or_insert_with(|| GroupAccumulator {
            source: diagnostic.source.clone(),
            code: group_code.clone(),
            message: group_code
                .as_ref()
                .map_or_else(|| Some(diagnostic.message.clone()), |_| None),
            count: 0,
            severity: diagnostic.severity,
            diagnostics: Vec::new(),
        });

        entry.count += 1;
        if diagnostic.severity > entry.severity {
            entry.severity = diagnostic.severity;
        }
        if entry.diagnostics.len() < sample_limit {
            entry.diagnostics.push(diagnostic.clone());
        }
    }

    let result = groups
        .into_values()
        .map(GroupAccumulator::into_group)
        .collect::<Vec<_>>();
    debug_assert_eq!(
        result.iter().map(|group| group.count).sum::<usize>(),
        diagnostics.len(),
        "group counts must sum to diagnostics length"
    );
    debug_assert!(
        result.iter().all(|group| group.count > 0),
        "materialized groups must not be empty"
    );
    debug_assert!(
        result
            .iter()
            .all(|group| group.diagnostics.len() <= group.count),
        "group samples cannot exceed group counts"
    );
    if let Some(limit) = max_diagnostics {
        debug_assert!(
            result.iter().all(|group| group.diagnostics.len() <= limit),
            "group samples must respect configured cap"
        );
    }
    result
}

/// Stable grouping key for digest buckets.
///
/// # Contract
/// Preconditions: built from a normalized diagnostic.
/// Postconditions: ordering is deterministic for stable group output.
/// Failure modes: constructing the value directly has no failure path.
/// Panics: none.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey
{
    /// Diagnostic source name.
    source: String,
    /// Code or message fallback value.
    discriminator: String,
    /// Whether the discriminator is a code rather than a message.
    has_code: bool,
}

/// Constructors for stable digest grouping keys.
///
/// # Contract
/// Preconditions: implemented for keys derived from normalized diagnostics.
/// Postconditions: constructors keep key ordering stable.
/// Failure modes: constructors have no error return path.
/// Panics: constructors do not panic.
impl GroupKey
{
    /// Construct a grouping key from one diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` is normalized; empty diagnostic codes are
    /// treated as absent.
    /// Postconditions: returns a key using the non-empty code when present and
    /// falling back to message text otherwise.
    /// Failure modes: none.
    /// Panics: none.
    #[must_use]
    fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        if let Some(code) = diagnostic.code.as_ref().filter(|value| !value.is_empty()) {
            return Self {
                source: diagnostic.source.clone(),
                discriminator: code.clone(),
                has_code: true,
            };
        }

        Self {
            source: diagnostic.source.clone(),
            discriminator: diagnostic.message.clone(),
            has_code: false,
        }
    }
}

/// Mutable group state while accumulating diagnostics.
///
/// # Contract
/// Preconditions: created only while folding diagnostics into a group.
/// Postconditions: `count` tracks the full group size while `diagnostics` holds
/// only the capped sample subset.
/// Failure modes: constructing the value directly has no failure path.
/// Panics: none.
#[derive(Debug)]
struct GroupAccumulator
{
    /// Diagnostic source name.
    source: String,
    /// Structured diagnostic code shared by the group, when present.
    code: Option<String>,
    /// Message fallback when the group has no structured code.
    message: Option<String>,
    /// Number of deduplicated diagnostics in the group.
    count: usize,
    /// Highest observed severity in this group.
    severity: Severity,
    /// Capped sample diagnostics for agent context.
    diagnostics: Vec<Diagnostic>,
}

/// Finalization behavior for accumulated digest groups.
///
/// # Contract
/// Preconditions: implemented for accumulators produced by group folding.
/// Postconditions: finalizers preserve accumulated counts and samples.
/// Failure modes: finalizers have no error return path.
/// Panics: finalizers document their debug-only assertions precisely.
impl GroupAccumulator
{
    /// Convert accumulated state into the public group model.
    ///
    /// # Contract
    /// Preconditions: the accumulator has been created from at least one
    /// diagnostic and `count` reflects the full represented group size.
    /// Postconditions: returns a group preserving the accumulated metadata and
    /// attaching deterministic explanation data.
    /// Failure modes: none.
    /// Panics: debug builds may panic if the accumulator represents no
    /// diagnostics or holds more samples than its count.
    #[must_use]
    fn into_group(self) -> Group
    {
        debug_assert!(
            self.count > 0,
            "group accumulator must represent at least one diagnostic"
        );
        debug_assert!(
            self.diagnostics.len() <= self.count,
            "sample diagnostics cannot exceed represented count"
        );
        let explain = explain_code(&self.source, self.code.as_deref());
        Group {
            source: self.source,
            code: self.code,
            message: self.message,
            count: self.count,
            severity: self.severity,
            diagnostics: self.diagnostics,
            explain,
        }
    }
}

/// Primary offset basis for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableFingerprintHasher`].
/// Postconditions: value remains stable across builds.
/// Failure modes: none.
/// Panics: none.
const FNV_OFFSET_PRIMARY: u64 = 0xcbf2_9ce4_8422_2325;

/// Primary prime for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableFingerprintHasher`].
/// Postconditions: value remains stable across builds.
/// Failure modes: none.
/// Panics: none.
const FNV_PRIME_PRIMARY: u64 = 0x0000_0100_0000_01b3;

/// Secondary offset basis for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableFingerprintHasher`].
/// Postconditions: value remains stable across builds.
/// Failure modes: none.
/// Panics: none.
const FNV_OFFSET_SECONDARY: u64 = 0x8422_2325_cbf2_9ce4;

/// Secondary prime for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableFingerprintHasher`].
/// Postconditions: value remains stable across builds.
/// Failure modes: none.
/// Panics: none.
const FNV_PRIME_SECONDARY: u64 = 0x1000_0000_0000_01b3;

/// Normalized bounded semantic fingerprint for exact-repeat removal.
///
/// The fingerprint deliberately excludes [`Diagnostic::raw`] so large adapter
/// payloads are never serialized or retained in deduplication keys. Semantic
/// fields that affect rendered duplicate identity are folded into stable
/// hash-like words with explicit field lengths and separators.
///
/// # Contract
/// Preconditions: built from a normalized diagnostic whose source and message
/// have already passed adapter/model validation.
/// Postconditions: ordering is deterministic and equal fingerprints represent
/// diagnostics with the same normalized semantic fingerprint over source, code,
/// severity, message, spans, and suggestions, regardless of preserved raw
/// payload differences.
/// Failure modes: constructing the value directly has no recoverable failure.
/// Panics: none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DiagnosticFingerprint
{
    /// Primary stable semantic hash word.
    primary: u64,
    /// Secondary stable semantic hash word.
    secondary: u64,
    /// Number of normalized bytes folded into the hash words.
    semantic_bytes: usize,
    /// Number of spans represented by the fingerprint.
    span_count: usize,
    /// Number of suggestions represented by the fingerprint.
    suggestion_count: usize,
}

/// Exact normalized semantic identity used to resolve rare fingerprint
/// collisions.
///
/// This value is stored only inside a bounded-key bucket and still excludes
/// [`Diagnostic::raw`]. It preserves exact semantic duplicate behavior without
/// putting long raw JSON serializations into ordered map keys.
///
/// # Contract
/// Preconditions: built from a normalized diagnostic.
/// Postconditions: equality compares only source, code, severity, message,
/// spans, and suggestions.
/// Failure modes: constructing the value directly has no recoverable failure;
/// allocation may abort through the global allocator.
/// Panics: none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticIdentity
{
    /// Normalized source tool or protocol family.
    source: String,
    /// Structured code supplied by the source tool.
    code: Option<String>,
    /// Normalized diagnostic severity.
    severity: Severity,
    /// Human-readable diagnostic message.
    message: String,
    /// Normalized source spans attached to the diagnostic.
    spans: Vec<Span>,
    /// Normalized source suggestions attached to the diagnostic.
    suggestions: Vec<Suggestion>,
}

/// Constructors and fingerprinting for exact diagnostic identities.
///
/// # Contract
/// Preconditions: implemented for identities derived from normalized
/// diagnostics.
/// Postconditions: helpers never inspect or serialize raw payloads.
/// Failure modes: constructors have no error return path; allocation may abort
/// through the global allocator.
/// Panics: constructors do not panic.
impl DiagnosticIdentity
{
    /// Construct an exact semantic identity from one diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` is normalized and may carry an arbitrary raw
    /// JSON payload.
    /// Postconditions: returns source, code, severity, message, spans, and
    /// suggestions while excluding [`Diagnostic::raw`].
    /// Failure modes: none are returned; allocation may abort through the
    /// global allocator.
    /// Panics: none.
    #[must_use]
    fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        Self {
            source: diagnostic.source.clone(),
            code: diagnostic.code.clone(),
            severity: diagnostic.severity,
            message: diagnostic.message.clone(),
            spans: diagnostic.spans.clone(),
            suggestions: diagnostic.suggestions.clone(),
        }
    }

    /// Compute the bounded map key for this exact semantic identity.
    ///
    /// # Contract
    /// Preconditions: `self` came from a normalized diagnostic.
    /// Postconditions: returns a deterministic bounded key over this identity's
    /// semantic fields.
    /// Failure modes: none.
    /// Panics: none.
    #[must_use]
    fn fingerprint(&self) -> DiagnosticFingerprint
    {
        let mut hasher = StableFingerprintHasher::new();
        hasher.write_str(self.source.as_str());
        hasher.write_optional_str(self.code.as_deref());
        hasher.write_severity(self.severity);
        hasher.write_str(self.message.as_str());
        hasher.write_usize(self.spans.len());
        for span in &self.spans {
            hasher.write_span(span);
        }
        hasher.write_usize(self.suggestions.len());
        for suggestion in &self.suggestions {
            hasher.write_suggestion(suggestion);
        }
        hasher.into_fingerprint(self.spans.len(), self.suggestions.len())
    }
}

/// Stable allocation-free hasher for semantic diagnostic identity fields.
///
/// # Contract
/// Preconditions: callers feed fields in a fixed schema order with explicit
/// lengths before variable text.
/// Postconditions: equal input streams produce equal hash state on every
/// supported platform; raw JSON payloads are not accepted by this helper.
/// Failure modes: none.
/// Panics: none.
#[derive(Debug, Clone, Copy)]
struct StableFingerprintHasher
{
    /// Primary stable hash state.
    primary: u64,
    /// Secondary stable hash state.
    secondary: u64,
    /// Number of normalized bytes folded into the hash states.
    semantic_bytes: usize,
}

/// Stable fingerprint hashing operations.
///
/// # Contract
/// Preconditions: methods are called in the schema order encoded by
/// [`DiagnosticIdentity::fingerprint`].
/// Postconditions: methods append tagged, length-delimited field data to the
/// stable hash stream without allocation.
/// Failure modes: none.
/// Panics: none.
impl StableFingerprintHasher
{
    /// Construct an empty stable fingerprint hasher.
    ///
    /// # Contract
    /// Preconditions: none.
    /// Postconditions: returns the documented offset-basis state.
    /// Failure modes: none.
    /// Panics: none.
    #[must_use]
    fn new() -> Self
    {
        Self {
            primary: FNV_OFFSET_PRIMARY,
            secondary: FNV_OFFSET_SECONDARY,
            semantic_bytes: 0,
        }
    }

    /// Finish hashing into a bounded diagnostic fingerprint key.
    ///
    /// # Contract
    /// Preconditions: all semantic fields have already been written.
    /// Postconditions: returns a key containing only fixed-size hash and count
    /// fields.
    /// Failure modes: none.
    /// Panics: none.
    #[must_use]
    fn into_fingerprint(
        self,
        span_count: usize,
        suggestion_count: usize,
    ) -> DiagnosticFingerprint
    {
        DiagnosticFingerprint {
            primary: self.primary,
            secondary: self.secondary,
            semantic_bytes: self.semantic_bytes,
            span_count,
            suggestion_count,
        }
    }

    /// Write a single tagged byte into both hash states.
    ///
    /// # Contract
    /// Preconditions: `byte` is part of the normalized fingerprint stream.
    /// Postconditions: hash states and normalized byte count advance
    /// deterministically.
    /// Failure modes: none.
    /// Panics: none.
    fn write_byte(
        &mut self,
        byte: u8,
    )
    {
        let word = u64::from(byte);
        self.primary ^= word;
        self.primary = self.primary.wrapping_mul(FNV_PRIME_PRIMARY);
        self.secondary = self.secondary.wrapping_mul(FNV_PRIME_SECONDARY);
        self.secondary ^= word.rotate_left(1);
        self.semantic_bytes = self.semantic_bytes.saturating_add(1);
    }

    /// Write a native length in portable little-endian form.
    ///
    /// Values that cannot fit in `u64` on wider-than-64-bit targets fold as
    /// `u64::MAX`, preserving the helper's infallible contract while keeping
    /// the hash stream stable for all representable Rust targets.
    ///
    /// # Contract
    /// Preconditions: `value` is a field length or count from normalized model
    /// data.
    /// Postconditions: exactly eight bytes are folded into the hash stream on
    /// every platform.
    /// Failure modes: none.
    /// Panics: none.
    fn write_usize(
        &mut self,
        value: usize,
    )
    {
        let stable_value = u64::try_from(value).unwrap_or(u64::MAX);
        for byte in stable_value.to_le_bytes() {
            self.write_byte(byte);
        }
    }

    /// Write an optional one-based coordinate.
    ///
    /// # Contract
    /// Preconditions: coordinates came from normalized model spans.
    /// Postconditions: absence and presence are distinct in the hash stream.
    /// Failure modes: none.
    /// Panics: none.
    fn write_optional_u32(
        &mut self,
        value: Option<u32>,
    )
    {
        match value {
            | Some(number) => {
                self.write_byte(1);
                for byte in number.to_le_bytes() {
                    self.write_byte(byte);
                }
            },
            | None => self.write_byte(0),
        }
    }

    /// Write UTF-8 text with a length delimiter.
    ///
    /// # Contract
    /// Preconditions: `value` is a normalized UTF-8 semantic field.
    /// Postconditions: text length and bytes are folded deterministically.
    /// Failure modes: none.
    /// Panics: none.
    fn write_str(
        &mut self,
        value: &str,
    )
    {
        self.write_usize(value.len());
        for byte in value.bytes() {
            self.write_byte(byte);
        }
    }

    /// Write optional UTF-8 text with a presence marker.
    ///
    /// # Contract
    /// Preconditions: `value` is absent or a normalized UTF-8 semantic field.
    /// Postconditions: `None` and `Some("")` remain distinct.
    /// Failure modes: none.
    /// Panics: none.
    fn write_optional_str(
        &mut self,
        value: Option<&str>,
    )
    {
        match value {
            | Some(text) => {
                self.write_byte(1);
                self.write_str(text);
            },
            | None => self.write_byte(0),
        }
    }

    /// Write normalized severity as a compact discriminator.
    ///
    /// # Contract
    /// Preconditions: `severity` is a valid model severity.
    /// Postconditions: the four severity variants have stable distinct bytes.
    /// Failure modes: none.
    /// Panics: none.
    fn write_severity(
        &mut self,
        severity: Severity,
    )
    {
        let value = match severity {
            | Severity::Hint => 0,
            | Severity::Info => 1,
            | Severity::Warning => 2,
            | Severity::Error => 3,
        };
        self.write_byte(value);
    }

    /// Write a normalized span.
    ///
    /// # Contract
    /// Preconditions: `span` was produced by adapter/model normalization.
    /// Postconditions: file and all optional coordinates are folded in stable
    /// field order.
    /// Failure modes: none.
    /// Panics: none.
    fn write_span(
        &mut self,
        span: &Span,
    )
    {
        self.write_str(span.file.as_str());
        self.write_optional_u32(span.line);
        self.write_optional_u32(span.column);
        self.write_optional_u32(span.end_line);
        self.write_optional_u32(span.end_column);
    }

    /// Write a normalized suggestion.
    ///
    /// # Contract
    /// Preconditions: `suggestion` was produced by adapter/model normalization.
    /// Postconditions: message, replacement, and optional span are folded in
    /// stable field order.
    /// Failure modes: none.
    /// Panics: none.
    fn write_suggestion(
        &mut self,
        suggestion: &Suggestion,
    )
    {
        self.write_str(suggestion.message.as_str());
        self.write_optional_str(suggestion.replacement.as_deref());
        match suggestion.span.as_ref() {
            | Some(span) => {
                self.write_byte(1);
                self.write_span(span);
            },
            | None => self.write_byte(0),
        }
    }
}

/// Unit coverage for digest semantic deduplication.
///
/// # Contract
/// Preconditions: compiled only for crate tests.
/// Postconditions: exercises private digest helpers without exposing test-only
/// API.
/// Failure modes: test failures return descriptive errors.
/// Panics: none.
#[cfg(test)]
mod tests
{
    use serde_json::json;

    use super::*;
    use crate::model::Protocol;

    /// Converts a test invariant into a fallible result.
    ///
    /// # Contract
    /// Preconditions: `message` describes the false condition.
    /// Postconditions: returns `Ok(())` when `condition` is true.
    /// Failure modes: false conditions return `message` as the test error.
    /// Panics: none.
    fn require(
        condition: bool,
        message: &'static str,
    ) -> Result<(), String>
    {
        if condition {
            return Ok(());
        }

        Err(String::from(message))
    }

    /// Builds one diagnostic that differs only by raw payload across calls.
    ///
    /// # Contract
    /// Preconditions: `raw_label` is the test marker to preserve in raw JSON.
    /// Postconditions: returns a normalized diagnostic with identical semantic
    /// fields for every label value.
    /// Failure modes: none are returned; allocation may abort through the
    /// global allocator.
    /// Panics: none.
    fn diagnostic_with_raw(raw_label: &'static str) -> Diagnostic
    {
        Diagnostic::new(
            "clippy",
            Some(String::from("clippy::option_used")),
            Severity::Warning,
            "used fallible option access",
        )
        .with_details(
            alloc::vec![Span::new(
                "src/main.rs",
                Some(2),
                Some(5),
                Some(2),
                Some(11),
            )],
            alloc::vec![Suggestion::new(
                "handle the option",
                Some(String::from("match value")),
                Some(Span::new(
                    "src/main.rs",
                    Some(2),
                    Some(5),
                    Some(2),
                    Some(11),
                )),
            )],
            Some(json!({ "payload": raw_label })),
        )
    }

    /// Verifies raw payload differences do not defeat semantic deduplication.
    ///
    /// # Contract
    /// Preconditions: digest construction accepts normalized diagnostics with
    /// arbitrary raw payloads.
    /// Postconditions: diagnostics that differ only by raw JSON collapse to one
    /// deduplicated digest entry.
    /// Failure modes: digest invariant failures return descriptive test errors.
    /// Panics: none.
    #[test]
    fn digest_deduplicates_without_raw_payload_identity() -> Result<(), String>
    {
        let digest = build_digest(
            alloc::vec![diagnostic_with_raw("first"), diagnostic_with_raw("second"),],
            Invocation::pipeline(Protocol::AifixJson, "unit-test"),
            None,
        );

        require(
            digest.counts.total == 1,
            "raw-only diagnostic differences should be deduplicated",
        )?;
        require(
            digest.diagnostics.len() == 1,
            "deduplicated diagnostic list should contain one semantic entry",
        )?;
        require(
            digest.groups.iter().map(|group| group.count).sum::<usize>() == 1,
            "deduplicated groups should represent one semantic entry",
        )?;

        Ok(())
    }

    /// Verifies semantic field differences remain distinct after hashing.
    ///
    /// # Contract
    /// Preconditions: digest construction receives diagnostics that differ in
    /// normalized message text.
    /// Postconditions: semantic differences are retained as separate
    /// deduplicated digest entries.
    /// Failure modes: digest invariant failures return descriptive test errors.
    /// Panics: none.
    #[test]
    fn digest_keeps_distinct_semantic_messages() -> Result<(), String>
    {
        let mut second = diagnostic_with_raw("second");
        second.message = String::from("different semantic message");
        let digest = build_digest(
            alloc::vec![diagnostic_with_raw("first"), second,],
            Invocation::pipeline(Protocol::AifixJson, "unit-test"),
            None,
        );

        require(
            digest.counts.total == 2,
            "semantic message differences should not be deduplicated",
        )?;
        require(
            digest.diagnostics.len() == 2,
            "deduplicated diagnostic list should keep semantic differences",
        )?;

        Ok(())
    }
}
