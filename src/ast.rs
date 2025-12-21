/// Top-level parsed token
#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    /// Macro call
    MacroCall(MacroCall<'a>),
    /// Positional argument reference: $1, $2, etc.
    Positional(usize),
    /// Literal text (whitespace, punctuation, quoted content, etc.)
    /// Empty arguments are represented as Literal("")
    Literal(&'a str),
    /// Grouped tokens (from quoted strings or multi-token arguments)
    Group(Group<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    pub lexeme: &'a str,
    pub tokens: Vec<Token<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MacroCall<'a> {
    pub name: &'a str,
    pub args: Vec<Token<'a>>,
}
