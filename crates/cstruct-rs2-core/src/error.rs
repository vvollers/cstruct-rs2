//! Failures, and which Python exception each one becomes.
//!
//! Which exception class a failure raises is part of the interface, not an
//! implementation detail. `dissect.*` code catches specific classes and branches
//! on them, so the variants here are chosen to line up one-to-one with what
//! upstream `dissect.cstruct` raises for the same input. The binding crate does
//! the actual translation, in one place, so the mapping cannot drift between
//! call sites.
//!
//! # End of input is control flow
//!
//! [`Error::EndOfStream`] is not exceptional. Reading records until the stream
//! runs out is the normal way `dissect` walks a run of them:
//!
//! ```python
//! while True:
//!     try:
//!         yield c_ntfs.ATTRIBUTE_RECORD_HEADER(stream)
//!     except EOFError:
//!         break
//! ```
//!
//! That makes it frequent rather than rare, which is why the variant carries no
//! payload and formats no message until something asks for one.

use std::fmt;

/// What went wrong while lexing, parsing, compiling, or decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The input ended before a complete value could be read.
    ///
    /// Becomes `EOFError`. No payload, because callers use this to end a loop
    /// rather than to report a problem — see the module header.
    EndOfStream,

    /// Input ended after a LEB128 continuation byte.
    ///
    /// This is still `EOFError`, but upstream gives the self-delimiting reader
    /// a specific message that distinguishes an incomplete value from an
    /// ordinary short fixed-width field.
    Leb128EndOfStream,

    /// The definition text could not be parsed.
    ///
    /// Maps to Python `ParserError`. Carries a line number because a schema is
    /// written by a human and the message is read by one.
    Parse {
        /// 1-based source line the failure was detected on.
        line: u32,
        /// What the parser expected and what it found.
        message: String,
    },

    /// Expression text could not be turned into tokens.
    ///
    /// Separate from [`Self::Parse`] because the two mean different things to
    /// whoever wrote the expression: `0x` is a malformed *literal*, while `1 +`
    /// is a well-formed literal in a malformed *expression*.
    ///
    /// Upstream draws the same line at the same place — `Expression("0x")`
    /// raises `LexerError` there — so this variant maps onto that class.
    Lex {
        /// What could not be lexed.
        message: String,
    },

    /// The definition referred to a type that was never defined.
    UnknownType {
        /// The name that could not be resolved.
        name: String,
    },

    /// A construct this implementation does not handle.
    ///
    /// Becomes `NotImplementedError`. The rule is that anything outside what
    /// this implementation supports fails loudly rather than quietly producing
    /// a value that differs from upstream — a refusal can be diagnosed, a wrong
    /// record cannot.
    Unsupported {
        /// The construct that is out of scope, named as a human would say it.
        construct: String,
    },

    /// A size, count, or offset taken from the input was not usable.
    ///
    /// Every length and offset comes from the data being decoded, which may be
    /// damaged or hostile, so this is an ordinary outcome rather than a sign of
    /// an internal fault.
    InvalidValue {
        /// What was wrong with the value.
        message: String,
    },

    /// A resource limit was hit while decoding.
    ///
    /// Stops damaged input from asking for unbounded work — a length field that
    /// declares four billion elements, or a self-referential type that would
    /// recurse until the stack runs out.
    LimitExceeded {
        /// Which limit was hit.
        limit: &'static str,
        /// The value that exceeded it.
        value: u64,
    },
}

impl Error {
    /// Name of the Python exception class this becomes.
    ///
    /// Lives here rather than in the binding crate for two reasons: the mapping
    /// is readable next to the variants it describes, and this crate's tests can
    /// check it without starting an interpreter.
    pub fn python_exception(&self) -> &'static str {
        match self {
            Self::EndOfStream | Self::Leb128EndOfStream => "EOFError",
            Self::Parse { .. } => "ParserError",
            Self::Lex { .. } => "LexerError",
            Self::UnknownType { .. } => "ResolveError",
            Self::Unsupported { .. } => "NotImplementedError",
            Self::InvalidValue { .. } => "ValueError",
            Self::LimitExceeded { .. } => "ValueError",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOfStream => write!(f, "unexpected end of stream"),
            Self::Leb128EndOfStream => {
                write!(f, "EOF reached, while final LEB128 byte was not yet read")
            }
            Self::Parse { line, message } => write!(f, "line {line}: {message}"),
            Self::Lex { message } => write!(f, "{message}"),
            // Upstream's exact wording, so a consumer that matches on the
            // message sees the same text from either implementation.
            Self::UnknownType { name } => write!(f, "Unknown type {name}"),
            Self::Unsupported { construct } => write!(
                f,
                "{construct} is not supported by this implementation of dissect.cstruct"
            ),
            Self::InvalidValue { message } => write!(f, "{message}"),
            Self::LimitExceeded { limit, value } => {
                write!(f, "{limit} limit exceeded: {value}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias for fallible core operations.
pub type Result<T> = std::result::Result<T, Error>;
