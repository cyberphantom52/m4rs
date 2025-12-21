use std::borrow::Cow;

use pest::{Parser, iterators::Pairs};
use pest_derive::Parser;

use crate::ast::{Group, MacroCall, Token};

#[derive(Parser)]
#[grammar = "src/m4.pest"]
pub struct M4Parser;

impl M4Parser {
    /// Parse M4 input into a list of tokens
    pub fn parse_input<'a>(input: &'a str) -> Result<Vec<Token<'a>>, pest::error::Error<Rule>> {
        let mut pairs: Pairs<'a, Rule> = M4Parser::parse(Rule::file, input)?;
        let file = pairs.next().expect("parser returned no file rule");

        Ok(file
            .into_inner()
            .filter_map(Self::parse_token)
            .collect::<Vec<_>>())
    }

    fn parse_token(pair: pest::iterators::Pair<Rule>) -> Option<Token> {
        match pair.as_rule() {
            Rule::token => {
                let inner = pair.into_inner().next()?;
                Self::parse_token(inner)
            }
            Rule::positional_argument => {
                let num: usize = pair.as_str()[1..].parse().unwrap_or(0);
                Some(Token::Positional(num))
            }
            Rule::macrocall => Self::parse_macrocall(pair).map(Token::MacroCall),
            Rule::quoted_group => Self::parse_group(pair).map(Token::Group),
            Rule::literal | Rule::WHITESPACE => Some(Token::Literal(Cow::Borrowed(pair.as_str()))),
            _ => None,
        }
    }

    fn parse_macrocall(pair: pest::iterators::Pair<Rule>) -> Option<MacroCall> {
        let mut inner = pair.into_inner();

        let name = inner.next().map(|p| Cow::Borrowed(p.as_str()))?;
        let args = inner.next().map(Self::parse_arguments).unwrap_or_default();

        Some(MacroCall { name, args })
    }

    fn parse_arguments(pair: pest::iterators::Pair<Rule>) -> Vec<Token> {
        pair.into_inner()
            .find(|p| p.as_rule() == Rule::argument_list)
            .into_iter()
            .flat_map(|arg_list| {
                arg_list.into_inner().filter_map(|p| match p.as_rule() {
                    Rule::argument => Self::parse_argument(p),
                    _ => None,
                })
            })
            .collect()
    }

    fn parse_argument(pair: pest::iterators::Pair<Rule>) -> Option<Token> {
        let lexeme = pair.as_str();
        let tokens: Vec<Token> = pair.into_inner().filter_map(Self::parse_token).collect();

        // If there's exactly one token, return it directly
        if tokens.len() == 1 {
            return tokens.into_iter().next();
        }

        // Multiple tokens -> wrap in a Group
        Some(Token::Group(Group {
            lexeme: Cow::Borrowed(lexeme),
            tokens,
        }))
    }

    fn parse_group(pair: pest::iterators::Pair<Rule>) -> Option<Group> {
        let lexeme = pair.as_str();
        let content = lexeme
            .strip_prefix('`')
            .and_then(|t| t.strip_suffix('\''))
            .unwrap_or("");

        match M4Parser::parse_input(content) {
            Ok(tokens) => Some(Group {
                lexeme: Cow::Borrowed(lexeme),
                tokens,
            }),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_define() {
        let input = "define(`foo', `bar')";
        let tokens = M4Parser::parse_input(input).unwrap();
        assert_eq!(tokens.len(), 1);

        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"define");
                assert_eq!(args.len(), 2);

                assert!(matches!(&args[0], Token::Group(grp) if grp.lexeme == "`foo'"));
                assert!(matches!(&args[1], Token::Group(grp) if grp.lexeme == "`bar'"));
            }
            _ => panic!("Expected MacroCall token"),
        }
    }

    #[test]
    fn test_parse_define_with_positional() {
        let input = "define(`greet', `Hello $1!')";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"define");
                assert_eq!(args.len(), 2);

                assert!(matches!(&args[0], Token::Group(grp) if grp.lexeme == "`greet'"));

                assert_eq!(
                    args[1],
                    Token::Group(Group {
                        lexeme: Cow::Borrowed("`Hello $1!'"),
                        tokens: vec![
                            Token::MacroCall(MacroCall {
                                name: Cow::Borrowed("Hello"),
                                args: vec![]
                            }),
                            Token::Literal(Cow::Borrowed(" ")),
                            Token::Positional(1),
                            Token::Literal(Cow::Borrowed("!")),
                        ],
                    })
                );
            }
            _ => panic!("Expected MacroCall token"),
        }
    }

    #[test]
    fn test_parse_ifelse() {
        let input = "ifelse(a, b, yes, no)";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"ifelse");
                assert_eq!(args.len(), 4);
                assert!(matches!(&args[0], Token::MacroCall(mc) if mc.name == "a"));
                assert!(matches!(&args[1], Token::MacroCall(mc) if mc.name == "b"));
                assert!(matches!(&args[2], Token::MacroCall(mc) if mc.name == "yes"));
                assert!(matches!(&args[3], Token::MacroCall(mc) if mc.name == "no"));
            }
            _ => panic!("Expected MacroCall token for ifelse"),
        }
    }

    #[test]
    fn test_parse_ifdef() {
        let input = "ifdef(`DEBUG', `debug mode', `release mode')";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"ifdef");
                assert_eq!(args.len(), 3);
                assert!(matches!(&args[0], Token::Group(_)));
                assert!(matches!(&args[1], Token::Group(_)));
                assert!(matches!(&args[2], Token::Group(_)));
            }
            _ => panic!("Expected MacroCall token for ifdef"),
        }
    }

    #[test]
    fn test_parse_nested_macro() {
        let input = "ifelse(a, b, c, ifelse(d, e, f))";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"ifelse");
                assert!(args.len() == 4);
                assert!(matches!(args.last(), Some(Token::MacroCall(_))));
            }
            _ => panic!("Expected MacroCall token for ifelse"),
        }
    }

    #[test]
    fn test_parse_quoted_string() {
        let input = "`hello world'";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::Group(g) => {
                assert_eq!(g.lexeme, "`hello world'");
                // Content should have tokens for "hello", " ", "world"
                assert!(g.tokens.len() >= 2);
            }
            _ => panic!("Expected Group token"),
        }
    }

    #[test]
    fn test_parse_unquoted_args() {
        let input = "ifelse(a, b, hello world, no)";
        let tokens = M4Parser::parse_input(input).unwrap();
        match &tokens[0] {
            Token::MacroCall(MacroCall { name, args }) => {
                assert_eq!(name, &"ifelse");
                assert_eq!(args.len(), 4);
                assert!(matches!(
                    &args[2],
                    Token::Group(Group {
                        lexeme,
                        ..
                    }) if lexeme == "hello world"
                ));
            }
            _ => panic!("Expected MacroCall token for ifelse"),
        }
    }
}
