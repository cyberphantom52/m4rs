use std::borrow::Cow;

/// Top-level parsed token
#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    /// Macro call
    MacroCall(MacroCall<'a>),
    /// Positional argument reference: $1, $2, etc.
    Positional(usize),
    /// Literal text (whitespace, punctuation, quoted content, etc.)
    /// Empty arguments are represented as Literal("")
    Literal(Cow<'a, str>),
    /// Grouped tokens (from quoted strings or multi-token arguments)
    Group(Group<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group<'a> {
    pub lexeme: Cow<'a, str>,
    pub tokens: Vec<Token<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MacroCall<'a> {
    pub name: Cow<'a, str>,
    pub args: Vec<Token<'a>>,
}

impl<'a> Token<'a> {
    /// Convert all borrowed strings to owned, making the token 'static
    pub fn into_owned(self) -> Token<'static> {
        match self {
            Token::MacroCall(mc) => Token::MacroCall(mc.into_owned()),
            Token::Positional(n) => Token::Positional(n),
            Token::Literal(s) => Token::Literal(Cow::Owned(s.into_owned())),
            Token::Group(g) => Token::Group(g.into_owned()),
        }
    }
}

impl<'a> Group<'a> {
    pub fn into_owned(self) -> Group<'static> {
        Group {
            lexeme: Cow::Owned(self.lexeme.into_owned()),
            tokens: self.tokens.into_iter().map(Token::into_owned).collect(),
        }
    }
}

impl<'a> MacroCall<'a> {
    pub fn into_owned(self) -> MacroCall<'static> {
        MacroCall {
            name: Cow::Owned(self.name.into_owned()),
            args: self.args.into_iter().map(Token::into_owned).collect(),
        }
    }
}
