//! Property coverage for diagnostic identity, signatures, and digest summaries.
//!
//! These tests generate normalized diagnostics through the public model API and
//! assert semantic invariants that should hold regardless of preserved raw tool
//! payloads.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "bounded property generators use small arithmetic to keep diagnostic spans readable"
    )
)]

extern crate alloc;

/// Property tests for public diagnostic digest and signature APIs.
#[cfg(test)]
mod tests
{
    use alloc::collections::BTreeSet;

    use aifix::digest::build_digest;
    use aifix::model::Diagnostic;
    use aifix::model::Invocation;
    use aifix::model::Protocol;
    use aifix::model::Severity;
    use aifix::model::Span;
    use aifix::model::Suggestion;
    use aifix::signature::DiagnosticSignature;
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::test_runner::TestCaseError;
    use serde_json::json;

    /// Raw-payload-independent diagnostic semantics used for expected identity.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct SemanticDiagnostic
    {
        /// Source tool or protocol family.
        source: String,
        /// Optional structured diagnostic code.
        code: Option<String>,
        /// Normalized severity.
        severity: Severity,
        /// Human-readable diagnostic message.
        message: String,
        /// Source locations attached to the diagnostic.
        spans: Vec<Span>,
        /// Suggested fixes attached to the diagnostic.
        suggestions: Vec<Suggestion>,
    }

    /// Returns a normalized diagnostic with caller-supplied raw evidence.
    fn diagnostic_with_raw(
        semantic: &SemanticDiagnostic,
        raw_marker: u16,
    ) -> Diagnostic
    {
        Diagnostic::new(
            semantic.source.clone(),
            semantic.code.clone(),
            semantic.severity,
            semantic.message.clone(),
        )
        .with_details(
            semantic.spans.clone(),
            semantic.suggestions.clone(),
            Some(json!({ "raw_marker": raw_marker })),
        )
    }

    /// Small source-tool names that keep group keys easy to shrink.
    fn source_name() -> impl Strategy<Value = String>
    {
        prop_oneof![Just("rustc"), Just("clippy"), Just("tsc"), Just("lsp")].prop_map(str::to_owned)
    }

    /// Optional structured codes with useful duplicate pressure.
    fn diagnostic_code() -> impl Strategy<Value = Option<String>>
    {
        prop::option::of(0_u16 .. 32_u16).prop_map(|code| code.map(|value| format!("E{value:04}")))
    }

    /// Human messages shared by code-less group fallbacks.
    fn diagnostic_message() -> impl Strategy<Value = String>
    {
        (0_u16 .. 48_u16).prop_map(|value| format!("generated diagnostic message {value}"))
    }

    /// Normalized severities generated with a stable distribution.
    fn severity() -> impl Strategy<Value = Severity>
    {
        prop_oneof![
            Just(Severity::Hint),
            Just(Severity::Info),
            Just(Severity::Warning),
            Just(Severity::Error),
        ]
    }

    /// Source spans with one-line, ordered coordinates.
    fn span() -> impl Strategy<Value = Span>
    {
        (
            0_u16 .. 12_u16,
            1_u32 .. 200_u32,
            1_u32 .. 120_u32,
            0_u32 .. 24_u32,
        )
            .prop_map(|(file, line, column, width)| {
                Span::new(
                    format!("src/generated_{file}.rs"),
                    Some(line),
                    Some(column),
                    Some(line),
                    Some(column + width),
                )
            })
    }

    /// Suggestions with optional replacements and optional spans.
    fn suggestion() -> impl Strategy<Value = Suggestion>
    {
        (
            0_u16 .. 32_u16,
            prop::option::of(0_u16 .. 32_u16),
            prop::option::of(span()),
        )
            .prop_map(|(message, replacement, span)| {
                Suggestion::new(
                    format!("apply generated suggestion {message}"),
                    replacement.map(|value| format!("replacement_{value}")),
                    span,
                )
            })
    }

    /// Semantic diagnostics bounded enough for fast shrinking.
    fn semantic_diagnostic() -> impl Strategy<Value = SemanticDiagnostic>
    {
        (
            source_name(),
            diagnostic_code(),
            severity(),
            diagnostic_message(),
            vec(span(), 0_usize .. 3_usize),
            vec(suggestion(), 0_usize .. 3_usize),
        )
            .prop_map(|(source, code, severity, message, spans, suggestions)| {
                SemanticDiagnostic {
                    source,
                    code,
                    severity,
                    message,
                    spans,
                    suggestions,
                }
            })
    }

    /// Digest sample caps exercised alongside generated diagnostics.
    fn digest_cap() -> impl Strategy<Value = Option<usize>>
    {
        prop::option::of(0_usize .. 6_usize)
    }

    /// Returns the canonical signature key with only hexadecimal words
    /// uppercased.
    fn uppercase_signature_hex(key: &str) -> String
    {
        format!(
            "aifix-v1-{hex}",
            hex = key.trim_start_matches("aifix-v1-").to_ascii_uppercase()
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 96,
            max_shrink_iters: 1024,
            .. ProptestConfig::default()
        })]

        /// Digest construction deduplicates semantic repeats, keeps counts
        /// consistent, caps group samples, and signatures ignore raw payloads.
        #[test]
        fn generated_diagnostics_preserve_digest_and_signature_invariants(
            semantics in vec(semantic_diagnostic(), 0_usize .. 18_usize),
            max_diagnostics in digest_cap(),
        ) {
            let mut diagnostics = Vec::new();
            let mut expected_semantics = BTreeSet::new();

            for semantic in &semantics {
                expected_semantics.insert(semantic.clone());
                let first = diagnostic_with_raw(semantic, 1);
                let second = diagnostic_with_raw(semantic, 2);
                let first_signature = DiagnosticSignature::from_diagnostic(&first);
                let second_signature = DiagnosticSignature::from_diagnostic(&second);
                let first_key = first_signature.as_key();
                let uppercase_hex_key = uppercase_signature_hex(&first_key);

                prop_assert_eq!(
                    first_signature,
                    second_signature,
                    "raw payload changes must not affect diagnostic signatures"
                );
                let canonical_first = DiagnosticSignature::canonical_key(&first_key)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                let canonical_uppercase = DiagnosticSignature::canonical_key(&uppercase_hex_key)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert_eq!(
                    canonical_first.as_str(),
                    first_key.as_str(),
                    "canonical signature keys must round-trip unchanged"
                );
                prop_assert_eq!(
                    canonical_uppercase.as_str(),
                    first_key.as_str(),
                    "uppercase hexadecimal words must canonicalize to lowercase"
                );

                diagnostics.push(first);
                diagnostics.push(second);
            }

            let digest = build_digest(
                diagnostics,
                Invocation::pipeline(Protocol::AifixJson, "property-input"),
                max_diagnostics,
            );
            let source_total = digest.counts.by_source.values().copied().sum::<usize>();
            let severity_total = digest.counts.by_severity.values().copied().sum::<usize>();
            let group_total = digest.groups.iter().map(|group| group.count).sum::<usize>();

            prop_assert_eq!(
                digest.counts.total,
                expected_semantics.len(),
                "digest total must equal the number of unique semantic diagnostics"
            );
            prop_assert_eq!(
                digest.diagnostics.len(),
                digest.counts.total,
                "top-level diagnostics must contain the full deduplicated set"
            );
            prop_assert_eq!(
                source_total,
                digest.counts.total,
                "source counts must account for every deduplicated diagnostic"
            );
            prop_assert_eq!(
                severity_total,
                digest.counts.total,
                "severity counts must account for every deduplicated diagnostic"
            );
            prop_assert_eq!(
                group_total,
                digest.counts.total,
                "group counts must account for every deduplicated diagnostic"
            );

            for group in &digest.groups {
                prop_assert!(
                    group.diagnostics.len() <= group.count,
                    "group samples must not exceed the represented group count"
                );
                if let Some(limit) = max_diagnostics {
                    prop_assert!(
                        group.diagnostics.len() <= limit,
                        "group samples must respect the requested cap"
                    );
                }
            }
        }
    }
}
