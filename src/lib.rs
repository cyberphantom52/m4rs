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
