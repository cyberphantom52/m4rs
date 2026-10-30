use std::io;

use thiserror::Error;

use crate::parser::Rule;

/// Errors produced while loading or expanding M4 input
#[derive(Debug, Error)]
pub enum M4Error {
    /// Input could not be parsed (boxed because pest errors are large)
    #[error(transparent)]
    Parse(Box<pest::error::Error<Rule>>),
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
}

impl From<pest::error::Error<Rule>> for M4Error {
    fn from(err: pest::error::Error<Rule>) -> Self {
        Self::Parse(Box::new(err))
    }
}
