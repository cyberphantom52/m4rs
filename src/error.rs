use std::io;

use thiserror::Error;

/// Errors produced while loading or expanding M4 input
#[derive(Debug, Error)]
pub enum M4Error {
    /// A macro source file could not be read
    #[error("failed to read {path}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    /// Expansion recursed deeper than the expander allows
    #[error("maximum expansion depth of {0} exceeded")]
    MaxDepthExceeded(usize),
    /// Too many macro calls were collecting their arguments at once
    #[error("nesting limit of {0} exceeded")]
    NestingLimitExceeded(usize),
    /// Input ended inside a quoted string
    #[error("end of input in quoted string")]
    UnterminatedQuote,
    /// Input ended inside a macro call's argument list
    #[error("end of input in argument list of `{0}`")]
    UnterminatedArguments(String),
    /// A builtin was given a non-numeric argument
    #[error("invalid number `{value}` in {builtin}")]
    InvalidNumber {
        builtin: &'static str,
        value: String,
    },
    /// An `eval` expression could not be evaluated
    #[error("eval: {reason} in `{expression}`")]
    Eval {
        expression: String,
        reason: &'static str,
    },
}
