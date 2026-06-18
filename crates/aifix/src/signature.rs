//! Stable diagnostic signatures for cache and MCP callers.
//!
//! Signatures are schema-versioned fingerprints over normalized diagnostic
//! semantics.  They deliberately exclude [`Diagnostic::raw`] so callers can
//! identify diagnostics without serializing large source-tool payloads.

use alloc::format;
use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use serde::Deserialize;
use serde::Serialize;

use crate::error::AifixError;
use crate::model::Diagnostic;
use crate::model::Severity;
use crate::model::Span;
use crate::model::Suggestion;

/// Version number embedded in every diagnostic signature string.
///
/// # Contract
/// Preconditions: callers use this constant only for the signature schema in
/// this module. Postconditions: generated strings begin with `aifix-v1`.
/// Failure modes: none. Panics: none.
pub const SIGNATURE_SCHEMA_VERSION: u8 = 1;

/// Prefix used for serialized schema-versioned diagnostic signatures.
///
/// # Contract
/// Preconditions: this prefix is paired with [`SIGNATURE_SCHEMA_VERSION`].
/// Postconditions: parsed and rendered signatures use the same literal prefix.
/// Failure modes: none. Panics: none.
pub const SIGNATURE_PREFIX: &str = "aifix-v1";

/// Primary offset basis for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableSignatureHasher`].
/// Postconditions: value remains stable across builds. Failure modes: none.
/// Panics: none.
const FNV_OFFSET_PRIMARY: u64 = 0xcbf2_9ce4_8422_2325;

/// Primary prime for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableSignatureHasher`].
/// Postconditions: value remains stable across builds. Failure modes: none.
/// Panics: none.
const FNV_PRIME_PRIMARY: u64 = 0x0000_0100_0000_01b3;

/// Secondary offset basis for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableSignatureHasher`].
/// Postconditions: value remains stable across builds. Failure modes: none.
/// Panics: none.
const FNV_OFFSET_SECONDARY: u64 = 0x8422_2325_cbf2_9ce4;

/// Secondary prime for the stable semantic fingerprint hash.
///
/// # Contract
/// Preconditions: constant is used only by [`StableSignatureHasher`].
/// Postconditions: value remains stable across builds. Failure modes: none.
/// Panics: none.
const FNV_PRIME_SECONDARY: u64 = 0x1000_0000_0000_01b3;

/// Public stable identity for one normalized diagnostic.
///
/// The string form is `aifix-v1-<16 hex primary>-<16 hex secondary>`.  The two
/// words are computed from source, code, severity, message, spans, and
/// suggestions in a fixed order with length delimiters; raw payloads are never
/// inspected.
///
/// # Contract
/// Preconditions: values are created with
/// [`DiagnosticSignature::from_diagnostic`] or [`FromStr`]. Postconditions:
/// display and serde use the canonical stable string form. Failure modes:
/// parsing rejects malformed prefixes, word counts, or non-hex words with
/// [`AifixError::InvalidArgument`]. Panics: none.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticSignature
{
    /// Primary stable semantic hash word.
    primary: u64,
    /// Secondary stable semantic hash word.
    secondary: u64,
}

impl DiagnosticSignature
{
    /// Construct a stable signature from one normalized diagnostic.
    ///
    /// # Contract
    /// Preconditions: `diagnostic` was normalized by an adapter or caller and
    /// may carry any raw JSON payload. Postconditions: returns a deterministic
    /// two-word fingerprint over semantic fields only. Failure modes: none.
    /// Panics: none.
    #[must_use]
    #[inline]
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self
    {
        let mut hasher = StableSignatureHasher::new();
        hasher.write_str(diagnostic.source.as_str());
        hasher.write_optional_str(diagnostic.code.as_deref());
        hasher.write_severity(diagnostic.severity);
        hasher.write_str(diagnostic.message.as_str());
        hasher.write_usize(diagnostic.spans.len());
        for span in &diagnostic.spans {
            hasher.write_span(span);
        }
        hasher.write_usize(diagnostic.suggestions.len());
        for suggestion in &diagnostic.suggestions {
            hasher.write_suggestion(suggestion);
        }
        hasher.into_signature()
    }

    /// Construct a signature directly from already-validated hash words.
    ///
    /// # Contract
    /// Preconditions: `primary` and `secondary` came from this module's stable
    /// hashing scheme or from a validated external cache key. Postconditions:
    /// stores both words unchanged. Failure modes: none. Panics: none.
    #[must_use]
    #[inline]
    pub const fn from_words(
        primary: u64,
        secondary: u64,
    ) -> Self
    {
        Self { primary, secondary }
    }

    /// Return the primary fingerprint word.
    ///
    /// # Contract
    /// Preconditions: `self` is a valid signature. Postconditions: returns the
    /// first fixed-size word used by the canonical string. Failure modes: none.
    /// Panics: none.
    #[must_use]
    #[inline]
    pub const fn primary(self) -> u64
    {
        self.primary
    }

    /// Return the secondary fingerprint word.
    ///
    /// # Contract
    /// Preconditions: `self` is a valid signature. Postconditions: returns the
    /// second fixed-size word used by the canonical string. Failure modes:
    /// none. Panics: none.
    #[must_use]
    #[inline]
    pub const fn secondary(self) -> u64
    {
        self.secondary
    }

    /// Render the canonical cache key string.
    ///
    /// # Contract
    /// Preconditions: `self` is a valid signature. Postconditions: returns the
    /// exact `aifix-v1-<16 hex>-<16 hex>` spelling accepted by [`FromStr`].
    /// Failure modes: allocation may abort through the global allocator; no
    /// recoverable error is returned. Panics: none.
    #[must_use]
    #[inline]
    pub fn as_key(self) -> String
    {
        format!(
            "{SIGNATURE_PREFIX}-{primary:016x}-{secondary:016x}",
            primary = self.primary,
            secondary = self.secondary
        )
    }

    /// Validate and canonicalize a signature string.
    ///
    /// # Contract
    /// Preconditions: `value` is caller-supplied cache or MCP text.
    /// Postconditions: returns the canonical lowercase string for a valid
    /// signature. Failure modes: returns [`AifixError::InvalidArgument`] when
    /// the input is malformed. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] when `value` is not a valid
    /// `aifix-v1-<16 hex>-<16 hex>` signature.
    #[inline]
    pub fn canonical_key(value: &str) -> Result<String, AifixError>
    {
        Self::from_str(value).map(Self::as_key)
    }
}

impl fmt::Display for DiagnosticSignature
{
    /// Format the signature using the canonical cache key string.
    ///
    /// # Contract
    /// Preconditions: `f` is writable by the formatting runtime.
    /// Postconditions: writes the same spelling returned by
    /// [`DiagnosticSignature::as_key`]. Failure modes: returns the
    /// formatter error if writing fails. Panics: none.
    ///
    /// # Errors
    /// Returns formatter errors when writing the canonical signature fails.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        write!(
            f,
            "{SIGNATURE_PREFIX}-{primary:016x}-{secondary:016x}",
            primary = self.primary,
            secondary = self.secondary
        )
    }
}

impl FromStr for DiagnosticSignature
{
    type Err = AifixError;

    /// Parse a canonical or uppercase-hex diagnostic signature string.
    ///
    /// # Contract
    /// Preconditions: `s` is caller-supplied cache or MCP text. Postconditions:
    /// returns a signature with the two parsed words for valid `aifix-v1`
    /// input. Failure modes: returns [`AifixError::InvalidArgument`] for
    /// malformed prefixes, segment lengths, segment counts, or non-hex
    /// words. Panics: none.
    ///
    /// # Errors
    /// Returns [`AifixError::InvalidArgument`] for malformed prefixes, segment
    /// counts, segment lengths, or non-hex hash words.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err>
    {
        let mut parts = s.trim().split('-');
        let Some(namespace) = parts.next()
        else {
            return Err(invalid_signature(s));
        };
        let Some(version) = parts.next()
        else {
            return Err(invalid_signature(s));
        };
        let Some(primary) = parts.next()
        else {
            return Err(invalid_signature(s));
        };
        let Some(secondary) = parts.next()
        else {
            return Err(invalid_signature(s));
        };
        if parts.next().is_some()
            || namespace != "aifix"
            || version != "v1"
            || primary.len() != 16
            || secondary.len() != 16
            || !primary.bytes().all(is_ascii_hex)
            || !secondary.bytes().all(is_ascii_hex)
        {
            return Err(invalid_signature(s));
        }

        let Ok(parsed_primary) = u64::from_str_radix(primary, 16)
        else {
            return Err(invalid_signature(s));
        };
        let Ok(parsed_secondary) = u64::from_str_radix(secondary, 16)
        else {
            return Err(invalid_signature(s));
        };
        Ok(Self {
            primary: parsed_primary,
            secondary: parsed_secondary,
        })
    }
}

impl Serialize for DiagnosticSignature
{
    /// Serialize the signature as its canonical cache key string.
    ///
    /// # Contract
    /// Preconditions: `serializer` accepts string values. Postconditions: emits
    /// the canonical signature string. Failure modes: returns serializer
    /// errors. Panics: none.
    ///
    /// # Errors
    /// Returns serializer errors when the canonical signature string cannot be
    /// emitted.
    #[inline]
    fn serialize<S>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_key().as_str())
    }
}

impl<'de> Deserialize<'de> for DiagnosticSignature
{
    /// Deserialize a validated diagnostic signature string.
    ///
    /// # Contract
    /// Preconditions: input contains a string. Postconditions: returns the
    /// parsed signature. Failure modes: reports invalid strings through serde's
    /// custom error mechanism. Panics: none.
    ///
    /// # Errors
    /// Returns deserializer errors for non-string input and custom serde errors
    /// for malformed signature strings.
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_str(value.as_str()).map_err(serde::de::Error::custom)
    }
}

/// Return an invalid-argument error for malformed signature text.
///
/// # Contract
/// Preconditions: `value` is the rejected caller input. Postconditions: returns
/// a typed error that describes the required signature shape. Failure modes:
/// allocation may abort through the global allocator. Panics: none.
#[must_use]
fn invalid_signature(value: &str) -> AifixError
{
    AifixError::invalid_argument(format!(
        "invalid diagnostic signature `{}`; expected {SIGNATURE_PREFIX}-<16 hex primary>-<16 hex secondary>",
        value.trim()
    ))
}

/// Return whether one byte is an ASCII hexadecimal digit.
///
/// # Contract
/// Preconditions: `byte` is a byte from caller input. Postconditions: returns
/// true only for `0-9`, `a-f`, or `A-F`. Failure modes: none. Panics: none.
#[must_use]
fn is_ascii_hex(byte: u8) -> bool
{
    byte.is_ascii_hexdigit()
}

/// Stable allocation-free hasher for semantic diagnostic fields.
///
/// # Contract
/// Preconditions: callers feed fields in the fixed schema order encoded by
/// [`DiagnosticSignature::from_diagnostic`]. Postconditions: equal semantic
/// streams produce equal hash words across platforms. Failure modes: none.
/// Panics: none.
#[derive(Debug, Clone, Copy)]
struct StableSignatureHasher
{
    /// Primary stable hash state.
    primary: u64,
    /// Secondary stable hash state.
    secondary: u64,
}

impl StableSignatureHasher
{
    /// Construct an empty stable signature hasher.
    ///
    /// # Contract
    /// Preconditions: none. Postconditions: returns the documented offset-basis
    /// states. Failure modes: none. Panics: none.
    #[must_use]
    fn new() -> Self
    {
        Self {
            primary: FNV_OFFSET_PRIMARY,
            secondary: FNV_OFFSET_SECONDARY,
        }
    }

    /// Finish hashing into a diagnostic signature.
    ///
    /// # Contract
    /// Preconditions: all semantic fields have already been written.
    /// Postconditions: returns a two-word signature for the accumulated stream.
    /// Failure modes: none. Panics: none.
    #[must_use]
    fn into_signature(self) -> DiagnosticSignature
    {
        DiagnosticSignature {
            primary: self.primary,
            secondary: self.secondary,
        }
    }

    /// Write a single byte into both hash states.
    ///
    /// # Contract
    /// Preconditions: `byte` is part of the normalized fingerprint stream.
    /// Postconditions: hash states advance deterministically. Failure modes:
    /// none. Panics: none.
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
    }

    /// Write a native length in portable little-endian form.
    ///
    /// # Contract
    /// Preconditions: `value` is a field length or count. Postconditions: folds
    /// exactly eight bytes on every platform, saturating unreachable wider
    /// targets to `u64::MAX`. Failure modes: none. Panics: none.
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
    /// Preconditions: `value` came from a normalized span. Postconditions:
    /// absence and presence are distinct in the hash stream. Failure modes:
    /// none. Panics: none.
    fn write_optional_u32(
        &mut self,
        value: Option<u32>,
    )
    {
        match value {
            | Some(number) => {
                self.write_byte(1_u8);
                for byte in number.to_le_bytes() {
                    self.write_byte(byte);
                }
            },
            | None => self.write_byte(0_u8),
        }
    }

    /// Write UTF-8 text with a length delimiter.
    ///
    /// # Contract
    /// Preconditions: `value` is a normalized semantic field. Postconditions:
    /// length and bytes are folded without allocating. Failure modes: none.
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
    /// Preconditions: `value` is absent or a normalized semantic field.
    /// Postconditions: `None` and `Some("")` remain distinct. Failure modes:
    /// none. Panics: none.
    fn write_optional_str(
        &mut self,
        value: Option<&str>,
    )
    {
        match value {
            | Some(text) => {
                self.write_byte(1_u8);
                self.write_str(text);
            },
            | None => self.write_byte(0_u8),
        }
    }

    /// Write normalized severity as a compact discriminator.
    ///
    /// # Contract
    /// Preconditions: `severity` is a valid model severity. Postconditions: the
    /// supported severity variants have stable distinct bytes. Failure modes:
    /// none. Panics: none.
    fn write_severity(
        &mut self,
        severity: Severity,
    )
    {
        let value = match severity {
            | Severity::Hint => 0_u8,
            | Severity::Info => 1_u8,
            | Severity::Warning => 2_u8,
            | Severity::Error => 3_u8,
        };
        self.write_byte(value);
    }

    /// Write a normalized source span.
    ///
    /// # Contract
    /// Preconditions: `span` was produced by adapter/model normalization.
    /// Postconditions: file and optional coordinates are folded in stable field
    /// order. Failure modes: none. Panics: none.
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
    /// stable field order. Failure modes: none. Panics: none.
    fn write_suggestion(
        &mut self,
        suggestion: &Suggestion,
    )
    {
        self.write_str(suggestion.message.as_str());
        self.write_optional_str(suggestion.replacement.as_deref());
        match suggestion.span.as_ref() {
            | Some(span) => {
                self.write_byte(1_u8);
                self.write_span(span);
            },
            | None => self.write_byte(0_u8),
        }
    }
}
