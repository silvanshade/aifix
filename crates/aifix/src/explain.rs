//! Deterministic local explanations for diagnostic code families.
//!
//! The adapter never performs network calls while explaining a code.  Each
//! explanation is a stable hint that helps an agent decide which reference to
//! consult and how much confidence to place in the code family classification.

use serde::Deserialize;
use serde::Serialize;

/// Local confidence state for an explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ExplainStatus
{
    /// The source and code match a well-known diagnostic namespace.
    Known,
    /// The source is known, but the exact code family is not structured.
    SourceKnown,
    /// Neither source nor code provided enough structure for classification.
    Unknown,
}

/// Deterministic explanation metadata attached to grouped diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Explain
{
    /// Stable reference token or URL for the diagnostic family.
    pub explain_ref: String,
    /// Confidence status for the local classification.
    pub status: ExplainStatus,
    /// Short agent-facing summary of the diagnostic family.
    pub summary: String,
}

/// Return deterministic explanation metadata for a diagnostic source/code pair.
///
/// # Contract
/// - requires: `source` and `code`, when present, are raw diagnostic labels
///   that may contain surrounding whitespace.
/// - ensures: returns a stable local classification without network access;
///   recognized families receive deterministic references and unknown families
///   receive the documented fallback.
/// - fails: none.
/// - panics: none.
#[must_use]
#[inline]
pub fn explain_code(
    source: &str,
    code: Option<&str>,
) -> Explain
{
    let source_lower = source.trim().to_ascii_lowercase();
    let code_text = code.map(str::trim).filter(|value| !value.is_empty());

    if let Some(value) = code_text {
        if is_clippy_source(&source_lower) {
            let lint = normalize_clippy_lint(value);
            return Explain::known(
                format!("https://rust-lang.github.io/rust-clippy/master/index.html#{lint}"),
                format!(
                    "Clippy lint {lint}; inspect the lint documentation and prefer the smallest source-level fix."
                ),
            );
        }

        if is_rust_source(&source_lower) {
            if is_rust_error_code(value) {
                return Explain::known(
                    format!("https://doc.rust-lang.org/error_codes/{value}.html"),
                    format!(
                        "Rust compiler error {value}; consult the stable rustc error index for the exact language rule and suggested fix."
                    ),
                );
            }

            return Explain::known(
                format!("rustc:{value}"),
                format!(
                    "Rust compiler lint {value}; inspect the rustc lint documentation, spans, and message text before changing behavior."
                ),
            );
        }

        if is_rust_error_code(value) {
            return Explain::known(
                format!("https://doc.rust-lang.org/error_codes/{value}.html"),
                format!(
                    "Rust compiler error {value}; consult the stable rustc error index for the exact language rule and suggested fix."
                ),
            );
        }

        if is_clippy_code(value) {
            let lint = normalize_clippy_lint(value);
            return Explain::known(
                format!("https://rust-lang.github.io/rust-clippy/master/index.html#{lint}"),
                format!(
                    "Clippy lint {lint}; inspect the lint documentation and prefer the smallest source-level fix."
                ),
            );
        }

        if source_allows_js_ts_code_fallback(&source_lower) {
            if let Some(ts_code) = typescript_code(value) {
                return Explain::known(
                    format!("typescript:{ts_code}"),
                    format!(
                        "TypeScript diagnostic {ts_code}; check type relationships, module resolution, and compiler option interactions before editing."
                    ),
                );
            }

            if is_oxlint_like(value) || is_oxlint_source(&source_lower) {
                return Explain::known(
                    format!("oxlint:{value}"),
                    format!(
                        "JavaScript/TypeScript lint rule {value}; preserve runtime behavior and apply the rule-specific style or correctness change."
                    ),
                );
            }
        }
    }

    if is_rust_source(&source_lower) {
        return Explain::source_known(
            "rustc:unknown",
            "Rust compiler diagnostic without a structured error code; use spans and message text as the primary evidence.",
        );
    }

    if is_clippy_source(&source_lower) {
        return Explain::source_known(
            "clippy:unknown",
            "Clippy diagnostic without a structured lint name; inspect the message and source context before changing behavior.",
        );
    }

    if source_lower.contains("typescript") || source_lower == "tsc" {
        return Explain::source_known(
            "typescript:unknown",
            "TypeScript diagnostic without a structured TS code; use the message, file, and compiler options as context.",
        );
    }

    if is_oxlint_source(&source_lower) || source_lower.contains("eslint") {
        return Explain::source_known(
            "lint:unknown",
            "Lint diagnostic without a recognized rule code; treat it as a local static-analysis finding and verify the rule intent.",
        );
    }

    Explain::unknown()
}

/// Constructors for deterministic explanation metadata.
impl Explain
{
    /// Construct a known diagnostic explanation.
    ///
    /// # Contract
    /// - requires: `explain_ref` and `summary` are already normalized for
    ///   agent-facing output.
    /// - ensures: returns an explanation with `Known` status and the provided
    ///   payloads unchanged.
    /// - fails: none.
    /// - panics: debug builds may panic if either payload is empty.
    #[must_use]
    fn known(
        explain_ref: String,
        summary: String,
    ) -> Self
    {
        debug_assert!(
            !explain_ref.is_empty(),
            "known explanation refs must be non-empty"
        );
        debug_assert!(!summary.is_empty(), "known summaries must be non-empty");
        Self {
            explain_ref,
            status: ExplainStatus::Known,
            summary,
        }
    }

    /// Construct a source-known fallback explanation.
    ///
    /// # Contract
    /// - requires: `explain_ref` and `summary` are static, non-empty
    ///   descriptions for a recognized diagnostic source.
    /// - ensures: returns an explanation with `SourceKnown` status and owned
    ///   copies of the provided text.
    /// - fails: none.
    /// - panics: debug builds may panic if either fallback payload is empty.
    #[must_use]
    fn source_known(
        explain_ref: &str,
        summary: &str,
    ) -> Self
    {
        debug_assert!(
            !explain_ref.is_empty(),
            "source-known explanation refs must be non-empty"
        );
        debug_assert!(
            !summary.is_empty(),
            "source-known summaries must be non-empty"
        );
        Self {
            explain_ref: explain_ref.to_owned(),
            status: ExplainStatus::SourceKnown,
            summary: summary.to_owned(),
        }
    }

    /// Construct the final unknown fallback explanation.
    #[must_use]
    fn unknown() -> Self
    {
        Self {
            explain_ref: "unknown".to_owned(),
            status: ExplainStatus::Unknown,
            summary: "Unrecognized diagnostic family; rely on the normalized message, spans, suggestions, and raw tool output when available.".to_owned(),
        }
    }
}

/// Return true when a normalized source identifies rustc diagnostics.
#[must_use]
fn is_rust_source(source: &str) -> bool
{
    source.contains("rustc") || source == "rust"
}

/// Return true when a normalized source identifies Clippy diagnostics.
#[must_use]
fn is_clippy_source(source: &str) -> bool
{
    source.contains("clippy")
}

/// Return true when a normalized source identifies oxlint diagnostics.
#[must_use]
fn is_oxlint_source(source: &str) -> bool
{
    source.contains("oxlint")
}

/// Return true when code-only JS/TS fallbacks are allowed for a source.
#[must_use]
fn source_allows_js_ts_code_fallback(source: &str) -> bool
{
    let is_js_ts_source = source.contains("typescript")
        || source == "tsc"
        || source.contains("javascript")
        || source == "js"
        || source == "ts"
        || is_oxlint_source(source)
        || source.contains("eslint");

    is_js_ts_source || !(is_rust_source(source) || is_clippy_source(source))
}

/// Return true when a code matches rustc's canonical `E####` shape.
#[must_use]
fn is_rust_error_code(value: &str) -> bool
{
    let bytes = value.as_bytes();
    let Some((prefix, digits)) = bytes.split_first()
    else {
        return false;
    };
    *prefix == b'E' && digits.len() == 4 && digits.iter().all(u8::is_ascii_digit)
}

/// Return true when a code is written as a Clippy lint path.
#[must_use]
fn is_clippy_code(value: &str) -> bool
{
    value.starts_with("clippy::") || value.starts_with("clippy/")
}

/// Normalize a Clippy lint code into the anchor used by Clippy's lint index.
#[must_use]
fn normalize_clippy_lint(value: &str) -> String
{
    value
        .trim_start_matches("clippy::")
        .trim_start_matches("clippy/")
        .replace('-', "_")
}

/// Extract TypeScript's canonical `TS####` code shape.
#[must_use]
fn typescript_code(value: &str) -> Option<String>
{
    let candidate = value.strip_prefix("TS").map_or(value, |stripped| stripped);
    if candidate.len() == 4 && candidate.as_bytes().iter().all(u8::is_ascii_digit) {
        return Some(format!("TS{candidate}"));
    }

    None
}

/// Return true for common lint rule names outside the Rust and TypeScript
/// spaces.
#[must_use]
fn is_oxlint_like(value: &str) -> bool
{
    let has_namespace = value.contains('/') || value.contains("::");
    let has_lint_words = value.contains('-') || value.contains('_');
    has_namespace || has_lint_words
}

/// Unit tests for deterministic diagnostic explanation classification.
#[cfg(test)]
mod tests
{
    use super::*;

    /// Verifies rustc lint labels are not classified as JavaScript lint rules.
    #[test]
    fn rustc_unused_variables_resolves_as_rust_lint()
    {
        let explanation = explain_code("rustc", Some("unused_variables"));

        assert_eq!(explanation.status, ExplainStatus::Known);
        assert_eq!(explanation.explain_ref, "rustc:unused_variables");
        assert!(explanation.summary.contains("Rust compiler lint"));
        assert!(!explanation.explain_ref.starts_with("oxlint:"));
    }

    /// Verifies the plain Rust source label receives rustc lint wording.
    #[test]
    fn rust_source_dead_code_resolves_as_rust_lint()
    {
        let explanation = explain_code("rust", Some("dead_code"));

        assert_eq!(explanation.status, ExplainStatus::Known);
        assert_eq!(explanation.explain_ref, "rustc:dead_code");
        assert!(explanation.summary.contains("rustc lint documentation"));
        assert!(!explanation.explain_ref.starts_with("oxlint:"));
    }

    /// Verifies Clippy sources accept bare and canonical lint names.
    #[test]
    fn clippy_source_resolves_bare_and_canonical_lints()
    {
        let bare = explain_code("clippy", Some("unwrap_used"));
        let canonical = explain_code("cargo clippy", Some("clippy::unwrap_used"));
        let slash = explain_code("clippy-driver", Some("clippy/unwrap_used"));

        assert_eq!(bare.status, ExplainStatus::Known);
        assert_eq!(
            bare.explain_ref,
            "https://rust-lang.github.io/rust-clippy/master/index.html#unwrap_used"
        );
        assert!(bare.summary.contains("Clippy lint unwrap_used"));
        assert_eq!(canonical.status, ExplainStatus::Known);
        assert_eq!(canonical.explain_ref, bare.explain_ref);
        assert_eq!(slash.status, ExplainStatus::Known);
        assert_eq!(slash.explain_ref, bare.explain_ref);
        assert!(!canonical.explain_ref.starts_with("oxlint:"));
    }

    /// Verifies unknown-source JavaScript lint codes retain oxlint behavior.
    #[test]
    fn unknown_source_lint_code_still_resolves_as_oxlint()
    {
        let explanation = explain_code("unknown-tool", Some("no-unused-vars"));

        assert_eq!(explanation.status, ExplainStatus::Known);
        assert_eq!(explanation.explain_ref, "oxlint:no-unused-vars");
        assert!(
            explanation
                .summary
                .contains("JavaScript/TypeScript lint rule")
        );
    }
}
