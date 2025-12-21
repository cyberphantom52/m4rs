use pest::Parser;
use pest_derive::Parser;

/// Top-level parsed token
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Macro call
    MacroCall { name: String, args: Vec<Vec<Token>> },
    /// Positional argument reference: $1, $2, etc.
    Positional(usize),
    /// Literal text (whitespace, punctuation, quoted content, etc.)
    /// Empty arguments are represented as Literal("")
    Literal(String),
}

#[derive(Parser)]
#[grammar = "src/m4.pest"]
pub struct M4Parser;

impl M4Parser {}

#[cfg(test)]
mod tests {
    use super::*;
    use pest::Parser;
}
