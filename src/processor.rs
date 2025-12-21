use std::collections::HashMap;
use std::io::{self, Read};

use crate::ast::*;
use crate::parser::M4Parser;

/// Registry of macro definitions (stores raw, unexpanded tokens)
#[derive(Debug, Default, Clone)]
pub struct MacroRegistry(HashMap<String, Vec<Token<'static>>>);

impl MacroRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load macro definitions from M4 source text.
    /// Expands the source - define() calls populate the registry as a side effect.
    pub fn load(&mut self, source: &str) -> Result<(), String> {
        let mut expander = Expander::new(self.clone());
        expander.expand(source)?;
        *self = expander.into_registry();
        Ok(())
    }

    /// Load macro definitions from a file
    pub fn load_file(&mut self, path: &str) -> Result<(), String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path, e))?;
        self.load(&source)
    }

    /// Register a macro definition (takes owned tokens)
    pub fn define(&mut self, name: String, body: Vec<Token<'static>>) {
        self.0.insert(name, body);
    }

    /// Get a macro definition by name
    pub fn get(&self, name: &str) -> Option<&Vec<Token<'static>>> {
        self.0.get(name)
    }

    /// Check if a macro is defined
    pub fn is_defined(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
}

/// M4 macro expander with recursive expansion
pub struct Expander {
    pub registry: MacroRegistry,
    max_depth: usize,
}

impl Expander {
    pub fn new(registry: MacroRegistry) -> Self {
        Self {
            registry,
            max_depth: 100,
        }
    }

    pub fn into_registry(self) -> MacroRegistry {
        self.registry
    }

    /// Expand all macros in the input text (main entry point)
    pub fn expand(&mut self, input: &str) -> Result<String, String> {
        let tokens = M4Parser::parse_input(input).map_err(|e| e.to_string())?;
        self.expand_tokens(&tokens)
    }

    /// Expand a list of tokens
    pub fn expand_tokens(&mut self, tokens: &[Token]) -> Result<String, String> {
        self.expand_tokens_with_depth(tokens, &[], 0)
    }

    fn expand_tokens_with_depth(
        &mut self,
        tokens: &[Token],
        args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        if depth > self.max_depth {
            return Err("Maximum expansion depth exceeded".to_string());
        }

        let mut result = String::new();
        for token in tokens {
            result.push_str(&self.expand_token(token, args, depth)?);
        }
        Ok(result)
    }

    /// Expand a single token
    fn expand_token(
        &mut self,
        token: &Token,
        args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        match token {
            Token::MacroCall(call) => self.expand_macro_call(call, args, depth),
            Token::Positional(n) => {
                if *n > 0 && *n <= args.len() {
                    Ok(args[*n - 1].clone())
                } else {
                    Ok(String::new())
                }
            }
            Token::Literal(s) => Ok(s.to_string()),
            Token::Group(g) => {
                // For quoted strings (lexeme starts with `), strip quotes and return content
                // This implements M4's quote-to-delay-expansion behavior
                let lexeme = g.lexeme.as_ref();
                if lexeme.starts_with('`') && lexeme.ends_with('\'') {
                    // Return the content without quotes (don't expand inner tokens)
                    Ok(lexeme[1..lexeme.len() - 1].to_string())
                } else {
                    // For unquoted groups (like multi-token arguments), expand inner tokens
                    self.expand_tokens_with_depth(&g.tokens, args, depth)
                }
            }
        }
    }

    /// Expand a macro call - core recursive logic
    fn expand_macro_call(
        &mut self,
        call: &MacroCall,
        parent_args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        let name = call.name.as_ref();

        // Handle builtin macros by name
        match name {
            "define" => {
                // define(name, body) - extract and store in registry
                if call.args.len() >= 2 {
                    // Expand the name (to resolve ifdef, ifelse, etc.)
                    let macro_name = self.expand_token(&call.args[0], parent_args, depth)?;
                    let macro_name = macro_name.trim().to_string();
                    // Store raw body tokens
                    let body = self.extract_body_tokens(&call.args[1]);
                    self.registry.define(macro_name, body);
                }
                Ok(String::new())
            }
            "ifelse" => self.expand_ifelse(&call.args, parent_args, depth),
            "ifdef" => self.expand_ifdef(&call.args, parent_args, depth),
            "dnl" => {
                // Discard rest of line (handled in expand)
                Ok(String::new())
            }
            _ => {
                // User-defined macro: recursively expand each argument
                let expanded_args = self.expand_arguments(&call.args, parent_args, depth)?;

                // Look up in registry
                if let Some(body) = self.registry.get(name) {
                    let body = body.clone();
                    // Expand the body with the expanded arguments
                    let expanded =
                        self.expand_tokens_with_depth(&body, &expanded_args, depth + 1)?;
                    // Rescan: parse and expand the result
                    self.rescan(&expanded, depth + 1)
                } else {
                    // Unknown macro - output as-is
                    if expanded_args.is_empty() {
                        Ok(name.to_owned())
                    } else {
                        Ok(format!("{}({})", name, expanded_args.join(", ")))
                    }
                }
            }
        }
    }

    /// Recursively expand macro call arguments
    ///
    /// For each argument:
    /// - If it's a MacroCall for a defined macro, descend into that macro's definition,
    ///   expand it fully, and use the result as the argument value
    /// - If it's a Group (quoted), strip quotes and use literal content
    /// - Otherwise, expand normally
    fn expand_arguments(
        &mut self,
        args: &[Token],
        parent_args: &[String],
        depth: usize,
    ) -> Result<Vec<String>, String> {
        let mut result = Vec::with_capacity(args.len());
        for arg in args {
            result.push(self.expand_argument(arg, parent_args, depth)?);
        }
        Ok(result)
    }

    /// Expand a single argument, recursively descending into macro definitions
    fn expand_argument(
        &mut self,
        arg: &Token,
        parent_args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        match arg {
            Token::MacroCall(call) => {
                // Check if this is a defined macro - if so, descend into its definition
                let name = call.name.as_ref();

                // Handle built-in macros normally
                if matches!(name, "define" | "ifelse" | "ifdef" | "dnl") {
                    return self.expand_macro_call(call, parent_args, depth);
                }

                if self.registry.is_defined(name) {
                    // Recursively expand this macro call
                    self.expand_macro_call(call, parent_args, depth)
                } else {
                    // Not a defined macro - expand as normal token
                    self.expand_token(arg, parent_args, depth)
                }
            }
            Token::Group(g) => {
                // Quoted string - strip quotes and return content (no expansion)
                let lexeme = g.lexeme.as_ref();
                if lexeme.starts_with('`') && lexeme.ends_with('\'') {
                    Ok(lexeme[1..lexeme.len() - 1].to_string())
                } else {
                    // Unquoted group - expand inner tokens
                    self.expand_tokens_with_depth(&g.tokens, parent_args, depth)
                }
            }
            _ => self.expand_token(arg, parent_args, depth),
        }
    }

    /// Extract text content from a token (for getting macro names, comparison values, etc.)
    fn extract_text(&self, token: &Token, parent_args: &[String]) -> Result<String, String> {
        match token {
            Token::Literal(s) => Ok(s.to_string()),
            Token::Positional(n) => {
                if *n > 0 && *n <= parent_args.len() {
                    Ok(parent_args[*n - 1].clone())
                } else {
                    Ok(String::new())
                }
            }
            Token::Group(g) => {
                // For groups, concatenate all inner text
                let mut result = String::new();
                for t in &g.tokens {
                    result.push_str(&self.extract_text(t, parent_args)?);
                }
                Ok(result)
            }
            Token::MacroCall(call) => {
                // For a simple identifier (no args), just return the name
                if call.args.is_empty() {
                    Ok(call.name.to_string())
                } else {
                    // Has args, treat as complex expression
                    Ok(String::new())
                }
            }
        }
    }

    /// Extract body tokens from an argument, converting to owned for storage
    fn extract_body_tokens(&self, token: &Token) -> Vec<Token<'static>> {
        match token {
            Token::Group(g) => g.tokens.iter().map(|t| t.clone().into_owned()).collect(),
            _ => vec![token.clone().into_owned()],
        }
    }

    fn expand_ifelse(
        &mut self,
        args: &[Token],
        parent_args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        // ifelse(a, b, then, d, e, then2, ..., else)
        // Process in groups of 3
        let mut i = 0;
        while i + 2 < args.len() {
            let a = self.expand_token(&args[i], parent_args, depth)?;
            let b = self.expand_token(&args[i + 1], parent_args, depth)?;
            // M4 trims whitespace for comparison
            if a.trim() == b.trim() {
                let result = self.expand_token(&args[i + 2], parent_args, depth)?;
                return Ok(result.trim().to_string());
            }
            i += 3;
        }

        // Remaining arg is the else clause
        if i < args.len() {
            let result = self.expand_token(&args[i], parent_args, depth)?;
            Ok(result.trim().to_string())
        } else {
            Ok(String::new())
        }
    }

    fn expand_ifdef(
        &mut self,
        args: &[Token],
        parent_args: &[String],
        depth: usize,
    ) -> Result<String, String> {
        // ifdef(name, then, else?)
        if args.is_empty() {
            return Ok(String::new());
        }

        // Extract the macro name without expanding (for ifdef, we check the name, not its value)
        let name = self.extract_text(&args[0], parent_args)?;
        let name = name.trim();

        if self.registry.is_defined(name) {
            if args.len() > 1 {
                let result = self.expand_token(&args[1], parent_args, depth)?;
                Ok(result.trim().to_string())
            } else {
                Ok(String::new())
            }
        } else if args.len() > 2 {
            let result = self.expand_token(&args[2], parent_args, depth)?;
            Ok(result.trim().to_string())
        } else {
            Ok(String::new())
        }
    }

    /// Rescan: parse the expanded text and expand again
    fn rescan(&mut self, text: &str, depth: usize) -> Result<String, String> {
        if depth > self.max_depth {
            return Err("Maximum expansion depth exceeded".to_string());
        }

        // Try to parse - if it fails, just return the text as-is
        match M4Parser::parse_input(text) {
            Ok(tokens) => self.expand_tokens_with_depth(&tokens, &[], depth),
            Err(_) => Ok(text.to_string()),
        }
    }
}

/// A reader wrapper that expands M4 macros on-the-fly
pub struct ExpandingReader<R: Read> {
    inner: R,
    registry: MacroRegistry,
    buffer: Vec<u8>,
    buffer_pos: usize,
    done: bool,
}

impl<R: Read> ExpandingReader<R> {
    pub fn new(inner: R, registry: MacroRegistry) -> Self {
        Self {
            inner,
            registry,
            buffer: Vec::new(),
            buffer_pos: 0,
            done: false,
        }
    }

    fn fill_buffer(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }

        // Read the entire input (for now - could be optimized for streaming)
        let mut input = String::new();
        self.inner.read_to_string(&mut input)?;

        let mut expander = Expander::new(self.registry.clone());
        match expander.expand(&input) {
            Ok(expanded) => {
                self.buffer = expanded.into_bytes();
                self.buffer_pos = 0;
            }
            Err(e) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
        }

        self.done = true;
        Ok(())
    }
}

impl<R: Read> Read for ExpandingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.buffer_pos >= self.buffer.len() && !self.done {
            self.fill_buffer()?;
        }

        if self.buffer_pos >= self.buffer.len() {
            return Ok(0);
        }

        let available = self.buffer.len() - self.buffer_pos;
        let to_copy = std::cmp::min(available, buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
        self.buffer_pos += to_copy;

        Ok(to_copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn test_simple_define() {
        let mut registry = MacroRegistry::new();
        registry.load("define(`foo', `bar')").unwrap();
        assert!(registry.is_defined("foo"));
    }

    #[test]
    fn test_simple_expansion() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "foo".to_string(),
            vec![Token::Literal(Cow::Owned("bar".to_string()))],
        );

        let mut expander = Expander::new(registry);
        let result = expander.expand("hello foo world").unwrap();
        assert_eq!(result, "hello bar world");
    }

    #[test]
    fn test_macro_with_args() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "greet".to_string(),
            vec![
                Token::Literal(Cow::Owned("Hello ".to_string())),
                Token::Positional(1),
                Token::Literal(Cow::Owned("!".to_string())),
            ],
        );

        let mut expander = Expander::new(registry);
        let result = expander.expand("greet(World)").unwrap();
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_nested_expansion() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "inner".to_string(),
            vec![Token::Literal(Cow::Owned("INNER".to_string()))],
        );
        registry.define(
            "outer".to_string(),
            vec![
                Token::Literal(Cow::Owned("before ".to_string())),
                Token::MacroCall(MacroCall {
                    name: Cow::Owned("inner".to_string()),
                    args: vec![],
                }),
                Token::Literal(Cow::Owned(" after".to_string())),
            ],
        );

        let mut expander = Expander::new(registry);
        let result = expander.expand("outer").unwrap();
        assert_eq!(result, "before INNER after");
    }

    #[test]
    fn test_ifelse() {
        let registry = MacroRegistry::new();
        let mut expander = Expander::new(registry);

        let result = expander
            .expand("ifelse(a, a, hello world, nello world)")
            .unwrap();
        assert_eq!(result, "hello world");

        let result = expander
            .expand("ifelse(a, b, hello world, nello world)")
            .unwrap();
        assert_eq!(result, "nello world");
    }

    #[test]
    fn test_ifdef() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "DEBUG".to_string(),
            vec![Token::Literal(Cow::Owned("1".to_string()))],
        );

        let mut expander = Expander::new(registry);
        let result = expander.expand("ifdef(`DEBUG', `yes', `no')").unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_ifdef_undefined() {
        let registry = MacroRegistry::new();
        let mut expander = Expander::new(registry);

        let result = expander.expand("ifdef(`UNDEFINED', `yes', `no')").unwrap();
        assert_eq!(result, "no");
    }

    #[test]
    fn test_quoted_string() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "foo".to_string(),
            vec![Token::Literal(Cow::Owned("bar".to_string()))],
        );

        let mut expander = Expander::new(registry);
        // Quoted string should not expand
        let result = expander.expand("`foo'").unwrap();
        assert_eq!(result, "foo");
    }

    #[test]
    fn test_dnl() {
        let registry = MacroRegistry::new();
        let mut expander = Expander::new(registry);

        let input = r#"hello dnl this is removed
world"#;
        let tokens = M4Parser::parse_input(input).unwrap();
        println!("{:?}", tokens);

        let result = expander.expand(input).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_expanding_reader() {
        let mut registry = MacroRegistry::new();
        registry.define(
            "foo".to_string(),
            vec![Token::Literal(Cow::Owned("bar".to_string()))],
        );

        let input = "hello foo world";

        let mut reader = ExpandingReader::new(input.as_bytes(), registry);

        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();

        assert_eq!(output, "hello bar world");
    }

    #[test]
    fn test_recursive_argument_expansion() {
        // Test that arguments which are themselves macros get expanded first
        let mut registry = MacroRegistry::new();
        registry.define(
            "inner".to_string(),
            vec![Token::Literal(Cow::Owned("EXPANDED".to_string()))],
        );
        registry.define(
            "wrapper".to_string(),
            vec![
                Token::Literal(Cow::Owned("[".to_string())),
                Token::Positional(1),
                Token::Literal(Cow::Owned("]".to_string())),
            ],
        );

        let mut expander = Expander::new(registry);
        // When we call wrapper(inner), inner should be expanded to EXPANDED first
        let result = expander.expand("wrapper(inner)").unwrap();
        assert_eq!(result, "[EXPANDED]");
    }

    #[test]
    fn test_lazy_macro_storage() {
        // Verify that macros referencing other macros work correctly
        // because definitions are stored raw and expanded on use
        let mut registry = MacroRegistry::new();
        registry.load("define(`foo', `bar')").unwrap();
        registry.load("define(`baz', `foo qux')").unwrap();

        let mut expander = Expander::new(registry);
        // When baz is expanded, foo inside its body should also expand
        let result = expander.expand("baz").unwrap();
        assert_eq!(result, "bar qux");
    }

    #[test]
    fn test_conditional_macro_name() {
        // Test that macro names can be conditionally defined using ifdef
        let mut registry = MacroRegistry::new();

        // First, define a flag
        registry.load("define(`HAS_FEATURE', `1')").unwrap();

        // Now define a macro whose name depends on the flag
        registry
            .load("define(ifdef(`HAS_FEATURE', `feature_impl', `fallback_impl'), `FEATURE_CODE')")
            .unwrap();

        // The macro should be defined as 'feature_impl' (not 'fallback_impl')
        assert!(registry.is_defined("feature_impl"));
        assert!(!registry.is_defined("fallback_impl"));

        let mut expander = Expander::new(registry);
        let result = expander.expand("feature_impl").unwrap();
        assert_eq!(result, "FEATURE_CODE");
    }
}
