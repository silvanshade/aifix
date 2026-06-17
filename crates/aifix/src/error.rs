//! Error model for the `aifix` library.
//!
//! Errors keep the boundary that produced them visible: configuration loading,
//! process execution, parser rejection, and serialization failures remain
//! distinct so the CLI can report useful failures without `anyhow`.

use alloc::format;
use alloc::string::String;
use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;

/// Convenient result type for `aifix` operations.
///
/// # Contract
/// - Preconditions: callers choose `T` and, optionally, a domain-specific error
///   type.
/// - Postconditions: aliases `core::result::Result<T, E>` with [`AifixError`]
///   as the default error.
/// - Failure modes: none; this is a type alias.
/// - Panics: none.
pub type Result<T, E = AifixError> = core::result::Result<T, E>;

/// Failure modes produced by `aifix`.
///
/// # Contract
/// - Preconditions: variants preserve the boundary-specific error context
///   supplied by callers.
/// - Postconditions: formatting exposes the category while source data remains
///   typed.
/// - Failure modes: none; values are inert until returned or displayed.
/// - Panics: none.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AifixError
{
    /// File-system IO failed, optionally with the path being read or written.
    #[error("io error: {source}")]
    Io
    {
        /// Path associated with the failed operation, when available.
        path: Option<Utf8PathBuf>,
        /// Original IO error returned by the standard library.
        source: io::Error,
    },

    /// JSON parsing or rendering failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// TOML configuration parsing failed.
    #[error("toml configuration error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    /// Configuration discovery or validation failed.
    #[error("configuration error: {0}")]
    Config(String),

    /// Tool process setup, execution, or output handling failed.
    #[error("process error: {0}")]
    Process(String),

    /// Diagnostic input could not be parsed as the requested protocol.
    #[error("parser error: {0}")]
    Parser(String),

    /// Bytes from the operating system were not valid UTF-8.
    #[error("utf-8 error: {0}")]
    Utf8(String),

    /// A caller provided an unsupported or inconsistent argument.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl AifixError
{
    /// Construct an IO error with no path context.
    ///
    /// # Contract
    /// - Preconditions: `source` is the original IO failure.
    /// - Postconditions: returns [`AifixError::Io`] with no path attached.
    /// - Failure modes: none; construction is infallible.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn io(source: io::Error) -> Self
    {
        Self::Io { path: None, source }
    }

    /// Construct an IO error with UTF-8 path context.
    ///
    /// # Contract
    /// - Preconditions: `path` identifies the operation that produced `source`.
    /// - Postconditions: returns [`AifixError::Io`] with `path` preserved.
    /// - Failure modes: none; construction is infallible.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn io_path(
        path: Utf8PathBuf,
        source: io::Error,
    ) -> Self
    {
        Self::Io {
            path: Some(path),
            source,
        }
    }

    /// Construct a configuration error from displayable text.
    ///
    /// # Contract
    /// - Preconditions: `message` describes the configuration failure.
    /// - Postconditions: stores `message.into()` in [`AifixError::Config`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn config<Message>(message: Message) -> Self
    where
        Message: Into<String>,
    {
        Self::Config(message.into())
    }

    /// Construct a process error from displayable text.
    ///
    /// # Contract
    /// - Preconditions: `message` describes process setup, execution, or output
    ///   handling.
    /// - Postconditions: stores `message.into()` in [`AifixError::Process`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn process<Message>(message: Message) -> Self
    where
        Message: Into<String>,
    {
        Self::Process(message.into())
    }

    /// Construct a process error for captured output that exceeds a byte cap.
    ///
    /// # Contract
    /// - Preconditions: `stream` names the stream being captured, `command`
    ///   names the executable boundary, and `limit` is the configured
    ///   per-stream byte limit.
    /// - Postconditions: returns [`AifixError::Process`] with a stable message
    ///   that identifies the stream, command, and limit.
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn output_limit(
        stream: &str,
        command: &str,
        limit: usize,
    ) -> Self
    {
        Self::Process(format!(
            "{stream} from `{command}` exceeded capture limit of {limit} bytes"
        ))
    }

    /// Construct a parser error from displayable text.
    ///
    /// # Contract
    /// - Preconditions: `message` describes the rejected diagnostic input.
    /// - Postconditions: stores `message.into()` in [`AifixError::Parser`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn parser<Message>(message: Message) -> Self
    where
        Message: Into<String>,
    {
        Self::Parser(message.into())
    }

    /// Construct a UTF-8 conversion error from displayable text.
    ///
    /// # Contract
    /// - Preconditions: `message` describes the invalid byte sequence boundary.
    /// - Postconditions: stores `message.into()` in [`AifixError::Utf8`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn utf8<Message>(message: Message) -> Self
    where
        Message: Into<String>,
    {
        Self::Utf8(message.into())
    }

    /// Construct an invalid-argument error from displayable text.
    ///
    /// # Contract
    /// - Preconditions: `message` describes the unsupported or inconsistent
    ///   argument.
    /// - Postconditions: stores `message.into()` in
    ///   [`AifixError::InvalidArgument`].
    /// - Failure modes: allocation may abort through the global allocator; no
    ///   recoverable error is returned.
    /// - Panics: none.
    #[must_use]
    #[inline]
    pub fn invalid_argument<Message>(message: Message) -> Self
    where
        Message: Into<String>,
    {
        Self::InvalidArgument(message.into())
    }
}
