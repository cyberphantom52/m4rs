//! A streaming M4 macro processor following GNU m4.
//!
//! Input is read through a stack of pushed-back strings. A macro's expansion
//! is pushed back and rescanned, so it can produce further macro calls or
//! complete a call whose arguments continue in the remaining input. Input is
//! split into words (`[A-Za-z_][A-Za-z0-9_]*`), quoted strings, comments and
//! single characters, so a macro name is recognized wherever it appears, for
//! example after `:` or `-`.

use std::collections::HashMap;
use std::io::{self, Read};

use crate::error::M4Error;
use crate::eval;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Builtin {
    Define,
    Undefine,
    Ifdef,
    Ifelse,
    Eval,
    Incr,
    Decr,
    Dnl,
    Shift,
    Len,
}

impl Builtin {
    const ALL: [(&'static str, Builtin); 10] = [
        ("define", Builtin::Define),
        ("undefine", Builtin::Undefine),
        ("ifdef", Builtin::Ifdef),
        ("ifelse", Builtin::Ifelse),
        ("eval", Builtin::Eval),
        ("incr", Builtin::Incr),
        ("decr", Builtin::Decr),
        ("dnl", Builtin::Dnl),
        ("shift", Builtin::Shift),
        ("len", Builtin::Len),
    ];

    /// GNU m4 recognizes these builtins only when they are called with
    /// arguments; a bare `define` is ordinary text. `dnl` works without them.
    fn needs_args(self) -> bool {
        self != Builtin::Dnl
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Macro {
    Builtin(Builtin),
    User(String),
}

/// Macro definitions: the builtins plus user macros, whose bodies are stored
/// unexpanded and expanded on each use.
#[derive(Debug, Clone)]
pub struct MacroRegistry(HashMap<String, Macro>);

impl Default for MacroRegistry {
    fn default() -> Self {
        Self(
            Builtin::ALL
                .iter()
                .map(|&(name, builtin)| (name.to_string(), Macro::Builtin(builtin)))
                .collect(),
        )
    }
}

impl MacroRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load macro definitions from M4 source text. The source is expanded
    /// and its output discarded; `define` calls populate the registry.
    pub fn load(&mut self, source: &str) -> Result<(), M4Error> {
        Scanner::new(self, Expander::DEFAULT_MAX_DEPTH).expand(source)?;
        Ok(())
    }

    /// Load macro definitions from a file
    pub fn load_file(&mut self, path: &str) -> Result<(), M4Error> {
        let source = std::fs::read_to_string(path).map_err(|source| M4Error::Io {
            path: path.to_string(),
            source,
        })?;
        self.load(&source)
    }

    /// Define a user macro with an unexpanded body.
    pub fn define(&mut self, name: impl Into<String>, body: impl Into<String>) {
        self.0.insert(name.into(), Macro::User(body.into()));
    }

    /// The unexpanded body of a user macro; `None` for builtins and
    /// undefined names.
    pub fn get(&self, name: &str) -> Option<&str> {
        match self.0.get(name) {
            Some(Macro::User(body)) => Some(body),
            _ => None,
        }
    }

    /// Check if a macro, builtin or user-defined, is defined
    pub fn is_defined(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
}

/// M4 macro expander
pub struct Expander {
    pub registry: MacroRegistry,
    max_depth: usize,
}

impl Expander {
    /// How many generations of rescanned expansions may produce further
    /// expansions before the expander assumes infinite recursion. GNU m4 has
    /// no such limit and loops forever on `define(`x', `x')x`; legitimate tail
    /// recursion, such as AOSP generating 1024 MLS categories, stays far below.
    pub const DEFAULT_MAX_DEPTH: usize = 65_536;

    /// How many macro calls may be collecting their arguments at once, GNU
    /// m4's default nesting limit.
    pub const MAX_NESTING: usize = 1024;

    pub fn new(registry: MacroRegistry) -> Self {
        Self {
            registry,
            max_depth: Self::DEFAULT_MAX_DEPTH,
        }
    }

    pub fn into_registry(self) -> MacroRegistry {
        self.registry
    }

    /// Expand all macros in the input text
    pub fn expand(&mut self, input: &str) -> Result<String, M4Error> {
        Scanner::new(&mut self.registry, self.max_depth).expand(input)
    }
}

/// Pushed-back input. `depth` counts the expansions that produced it.
struct Frame {
    text: String,
    pos: usize,
    depth: usize,
}

enum Token {
    /// A word and the expansion depth of the input it was read from.
    Word(String, usize),
    /// A quoted string, without its outermost quotes.
    Quoted(String),
    /// A `#` comment through its newline, copied verbatim.
    Comment(String),
    Char(char),
    Eof,
}

fn is_word_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A macro call whose arguments are still being read.
struct PendingCall {
    name: String,
    found: Macro,
    /// Expansion depth of the input the name was read from.
    depth: usize,
    args: Vec<String>,
    current: String,
    /// Unquoted parentheses opened inside the current argument.
    parens: usize,
}

/// Where text read now belongs: the output, or the argument being collected.
fn sink<'a>(out: &'a mut String, calls: &'a mut [PendingCall]) -> &'a mut String {
    match calls.last_mut() {
        Some(call) => &mut call.current,
        None => out,
    }
}

struct Scanner<'r> {
    registry: &'r mut MacroRegistry,
    input: Vec<Frame>,
    max_depth: usize,
}

impl<'r> Scanner<'r> {
    fn new(registry: &'r mut MacroRegistry, max_depth: usize) -> Self {
        Self {
            registry,
            input: Vec::new(),
            max_depth,
        }
    }

    /// Expand `input`. Calls collecting arguments are kept on an explicit
    /// stack rather than the Rust call stack, so nesting depth is bounded by
    /// [`Expander::MAX_NESTING`], not by the thread's stack size.
    fn expand(mut self, input: &str) -> Result<String, M4Error> {
        self.input.push(Frame {
            text: input.to_string(),
            pos: 0,
            depth: 0,
        });

        let mut out = String::with_capacity(input.len());
        let mut calls: Vec<PendingCall> = Vec::new();
        // GNU m4 skips unquoted whitespace at the start of each argument.
        let mut at_arg_start = false;

        loop {
            if at_arg_start {
                while self.peek().is_some_and(char::is_whitespace) {
                    self.next_char();
                }
                at_arg_start = false;
            }

            match self.next_token()? {
                Token::Eof => {
                    return match calls.pop() {
                        Some(call) => Err(M4Error::UnterminatedArguments(call.name)),
                        None => Ok(out),
                    };
                }
                Token::Word(word, depth) => match self.callable(&word) {
                    Some(found) if self.peek() == Some('(') => {
                        self.next_char();
                        if calls.len() == Expander::MAX_NESTING {
                            return Err(M4Error::NestingLimitExceeded(Expander::MAX_NESTING));
                        }
                        calls.push(PendingCall {
                            name: word,
                            found,
                            depth,
                            args: Vec::new(),
                            current: String::new(),
                            parens: 0,
                        });
                        at_arg_start = true;
                    }
                    Some(found) => {
                        let expansion = self.apply(&word, found, &[])?;
                        self.push(expansion, depth + 1)?;
                    }
                    None => sink(&mut out, &mut calls).push_str(&word),
                },
                Token::Quoted(text) | Token::Comment(text) => {
                    sink(&mut out, &mut calls).push_str(&text)
                }
                Token::Char(c) => {
                    let Some(call) = calls.last_mut() else {
                        out.push(c);
                        continue;
                    };
                    match c {
                        '(' => {
                            call.parens += 1;
                            call.current.push(c);
                        }
                        ')' if call.parens == 0 => {
                            let call = calls.pop().expect("a call is pending");
                            let mut args = call.args;
                            args.push(call.current);
                            let expansion = self.apply(&call.name, call.found, &args)?;
                            self.push(expansion, call.depth + 1)?;
                        }
                        ')' => {
                            call.parens -= 1;
                            call.current.push(c);
                        }
                        ',' if call.parens == 0 => {
                            call.args.push(std::mem::take(&mut call.current));
                            at_arg_start = true;
                        }
                        _ => call.current.push(c),
                    }
                }
            }
        }
    }

    /// Drop exhausted frames and return the next character without
    /// consuming it.
    fn peek(&mut self) -> Option<char> {
        while let Some(frame) = self.input.last() {
            if let Some(c) = frame.text[frame.pos..].chars().next() {
                return Some(c);
            }
            self.input.pop();
        }
        None
    }

    fn next_char(&mut self) -> Option<char> {
        let c = self.peek()?;
        let frame = self.input.last_mut().expect("peek found a frame");
        frame.pos += c.len_utf8();
        Some(c)
    }

    fn push(&mut self, text: String, depth: usize) -> Result<(), M4Error> {
        if depth > self.max_depth {
            return Err(M4Error::MaxDepthExceeded(self.max_depth));
        }
        if !text.is_empty() {
            self.input.push(Frame {
                text,
                pos: 0,
                depth,
            });
        }
        Ok(())
    }

    fn next_token(&mut self) -> Result<Token, M4Error> {
        let Some(c) = self.peek() else {
            return Ok(Token::Eof);
        };

        if is_word_start(c) {
            let depth = self.input.last().map_or(0, |frame| frame.depth);
            let mut word = String::new();
            while let Some(c) = self.peek().filter(|&c| is_word_char(c)) {
                word.push(c);
                self.next_char();
            }
            return Ok(Token::Word(word, depth));
        }

        self.next_char();
        match c {
            '`' => {
                let mut text = String::new();
                let mut level = 1;
                loop {
                    let c = self.next_char().ok_or(M4Error::UnterminatedQuote)?;
                    match c {
                        '`' => level += 1,
                        '\'' => {
                            level -= 1;
                            if level == 0 {
                                return Ok(Token::Quoted(text));
                            }
                        }
                        _ => {}
                    }
                    text.push(c);
                }
            }
            '#' => {
                let mut text = String::from('#');
                while let Some(c) = self.next_char() {
                    text.push(c);
                    if c == '\n' {
                        break;
                    }
                }
                Ok(Token::Comment(text))
            }
            _ => Ok(Token::Char(c)),
        }
    }

    /// The macro `word` names, if this occurrence is a call.
    fn callable(&mut self, word: &str) -> Option<Macro> {
        let found = self.registry.0.get(word)?.clone();
        if let Macro::Builtin(builtin) = found
            && builtin.needs_args()
            && self.peek() != Some('(')
        {
            return None;
        }
        Some(found)
    }

    /// The expansion of a call with its collected arguments.
    fn apply(&mut self, name: &str, found: Macro, args: &[String]) -> Result<String, M4Error> {
        match found {
            Macro::User(body) => Ok(substitute(&body, name, args)),
            Macro::Builtin(builtin) => self.builtin(builtin, args),
        }
    }

    fn builtin(&mut self, builtin: Builtin, args: &[String]) -> Result<String, M4Error> {
        let arg = |i: usize| args.get(i).map_or("", String::as_str);

        Ok(match builtin {
            Builtin::Define => {
                if !arg(0).is_empty() {
                    self.registry.define(arg(0), arg(1));
                }
                String::new()
            }
            Builtin::Undefine => {
                for name in args {
                    self.registry.0.remove(name);
                }
                String::new()
            }
            Builtin::Ifdef => {
                if self.registry.is_defined(arg(0)) {
                    arg(1).to_string()
                } else {
                    arg(2).to_string()
                }
            }
            Builtin::Ifelse => ifelse(args).to_string(),
            Builtin::Eval => {
                let value = eval::evaluate(arg(0)).map_err(|reason| M4Error::Eval {
                    expression: arg(0).to_string(),
                    reason,
                })?;
                let radix = parse_int("eval", arg(1).trim(), 10)?;
                let width = parse_int("eval", arg(2).trim(), 1)?;
                if !(2..=36).contains(&radix) || width < 0 {
                    return Err(M4Error::InvalidNumber {
                        builtin: "eval",
                        value: format!("{}, {}", arg(1), arg(2)),
                    });
                }
                eval::format(value, radix as u32, width as usize)
            }
            Builtin::Incr => parse_int("incr", arg(0).trim(), 0)?
                .wrapping_add(1)
                .to_string(),
            Builtin::Decr => parse_int("decr", arg(0).trim(), 0)?
                .wrapping_sub(1)
                .to_string(),
            Builtin::Dnl => {
                while self.next_char().is_some_and(|c| c != '\n') {}
                String::new()
            }
            Builtin::Shift => quote_all(args.get(1..).unwrap_or_default()),
            Builtin::Len => arg(0).chars().count().to_string(),
        })
    }
}

/// Parse a builtin's numeric argument; an empty argument takes `default`.
fn parse_int(builtin: &'static str, value: &str, default: i32) -> Result<i32, M4Error> {
    if value.is_empty() {
        return Ok(default);
    }
    value.parse().map_err(|_| M4Error::InvalidNumber {
        builtin,
        value: value.to_string(),
    })
}

/// `ifelse(a, b, equal, [a2, b2, equal2, ...,] [otherwise])`
fn ifelse(args: &[String]) -> &str {
    let mut rest = args;
    loop {
        match rest {
            [a, b, equal, ..] if a == b => return equal,
            [_, _, _, otherwise] => return otherwise,
            [_, _, _, more @ ..] if more.len() > 1 => rest = more,
            _ => return "",
        }
    }
}

/// Arguments quoted and joined with commas, as `$@` and `shift` produce.
fn quote_all(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("`{arg}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Substitute `$0`-`$N`, `$#`, `$*` and `$@` throughout a macro body,
/// including inside quotes, before the expansion is rescanned.
fn substitute(body: &str, name: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some(d) if d.is_ascii_digit() => {
                let mut index = 0usize;
                while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                    index = index.saturating_mul(10).saturating_add(d as usize);
                    chars.next();
                }
                if index == 0 {
                    out.push_str(name);
                } else if let Some(arg) = args.get(index - 1) {
                    out.push_str(arg);
                }
            }
            Some('#') => {
                chars.next();
                out.push_str(&args.len().to_string());
            }
            Some('*') => {
                chars.next();
                out.push_str(&args.join(","));
            }
            Some('@') => {
                chars.next();
                out.push_str(&quote_all(args));
            }
            _ => out.push('$'),
        }
    }
    out
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

    fn expand(macros: &str, input: &str) -> String {
        let mut registry = MacroRegistry::new();
        registry.load(macros).unwrap();
        Expander::new(registry).expand(input).unwrap()
    }

    #[test]
    fn test_simple_define() {
        let mut registry = MacroRegistry::new();
        registry.load("define(`foo', `bar')").unwrap();
        assert!(registry.is_defined("foo"));
        assert_eq!(registry.get("foo"), Some("bar"));
    }

    #[test]
    fn test_simple_expansion() {
        let mut registry = MacroRegistry::new();
        registry.define("foo", "bar");

        let mut expander = Expander::new(registry);
        let result = expander.expand("hello foo world").unwrap();
        assert_eq!(result, "hello bar world");
    }

    #[test]
    fn test_macro_with_args() {
        assert_eq!(
            expand("define(`greet', `Hello $1!')", "greet(World)"),
            "Hello World!"
        );
    }

    #[test]
    fn test_nested_expansion() {
        assert_eq!(
            expand(
                "define(`inner', `INNER')define(`outer', `before inner after')",
                "outer"
            ),
            "before INNER after"
        );
    }

    #[test]
    fn test_ifelse() {
        let mut expander = Expander::new(MacroRegistry::new());

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
    fn test_ifelse_chains_and_comment_form() {
        assert_eq!(expand("", "ifelse(a, b, 1, a, a, 2, 3)"), "2");
        assert_eq!(expand("", "ifelse(a, b, 1, c, d, 2, 3)"), "3");
        assert_eq!(expand("", "ifelse(a, b, 1, c, d, 2)"), "");
        assert_eq!(expand("", "ifelse(`a comment, ignored')x"), "x");
    }

    #[test]
    fn test_ifdef() {
        let mut registry = MacroRegistry::new();
        registry.define("DEBUG", "1");

        let mut expander = Expander::new(registry);
        let result = expander.expand("ifdef(`DEBUG', `yes', `no')").unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_ifdef_undefined() {
        let mut expander = Expander::new(MacroRegistry::new());
        let result = expander.expand("ifdef(`UNDEFINED', `yes', `no')").unwrap();
        assert_eq!(result, "no");
    }

    #[test]
    fn test_rescan_preserves_parent_args() {
        assert_eq!(
            expand(
                "define(`pass_through', `$1')\ndefine(`outer', `pass_through(`$1')')",
                "outer(value)"
            ),
            "value"
        );
    }

    #[test]
    fn test_brace_set_macro_argument() {
        assert_eq!(
            expand(
                "define(`get_prop', `allow $1 $2:file { getattr open read map };')",
                "get_prop(hal_cryptoeng_oplus, { boot_status_prop serialno_prop })"
            ),
            "allow hal_cryptoeng_oplus { boot_status_prop serialno_prop }:file { getattr open read map };"
        );
    }

    #[test]
    fn test_quoted_string() {
        // Quoted text is not expanded; one level of quotes is removed.
        assert_eq!(expand("define(`foo', `bar')", "`foo' ``foo''"), "foo `foo'");
    }

    #[test]
    fn test_dnl() {
        let mut expander = Expander::new(MacroRegistry::new());
        let result = expander.expand("hello dnl this is removed\nworld").unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_expanding_reader() {
        let mut registry = MacroRegistry::new();
        registry.define("foo", "bar");

        let mut reader = ExpandingReader::new("hello foo world".as_bytes(), registry);
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();

        assert_eq!(output, "hello bar world");
    }

    #[test]
    fn test_recursive_argument_expansion() {
        assert_eq!(
            expand(
                "define(`inner', `EXPANDED')define(`wrapper', `[$1]')",
                "wrapper(inner)"
            ),
            "[EXPANDED]"
        );
    }

    #[test]
    fn test_lazy_macro_storage() {
        let mut registry = MacroRegistry::new();
        registry.load("define(`foo', `bar')").unwrap();
        registry.load("define(`baz', `foo qux')").unwrap();

        let mut expander = Expander::new(registry);
        assert_eq!(expander.expand("baz").unwrap(), "bar qux");
    }

    #[test]
    fn test_conditional_macro_name() {
        let mut registry = MacroRegistry::new();
        registry.load("define(`HAS_FEATURE', `1')").unwrap();
        registry
            .load("define(ifdef(`HAS_FEATURE', `feature_impl', `fallback_impl'), `FEATURE_CODE')")
            .unwrap();

        assert!(registry.is_defined("feature_impl"));
        assert!(!registry.is_defined("fallback_impl"));

        let mut expander = Expander::new(registry);
        assert_eq!(expander.expand("feature_impl").unwrap(), "FEATURE_CODE");
    }

    #[test]
    fn test_names_are_recognized_inside_other_text() {
        // `0x1` is `0` then the word `x1`; `a:x` is `a`, `:` and the macro `x`.
        assert_eq!(expand("define(`x', `1')", "0x1 x-x a:x"), "0x1 1-1 a:1");
        assert_eq!(
            expand("define(`LAST', `0x000089ff')", "0x000089f0-LAST"),
            "0x000089f0-0x000089ff"
        );
    }

    #[test]
    fn test_positional_args_substituted_inside_quotes() {
        assert_eq!(
            expand(
                "define(`trans', `ifelse($1, `init', `', `allow $3 $1:process sigchld;')')",
                "trans(vold, blkid_exec, blkid)"
            ),
            "allow blkid vold:process sigchld;"
        );
    }

    #[test]
    fn test_argument_whitespace_and_counts() {
        assert_eq!(
            expand("define(`w', `<$1|$2>')", "w(  x ,\n  y )"),
            "<x |y >"
        );
        assert_eq!(
            expand("define(`n', `$#:$*:$@')", "n(a,`b,c')"),
            "2:a,b,c:a,b,c"
        );
        assert_eq!(expand("define(`n', `$#')", "n n()"), "0 1");
        assert_eq!(
            expand("define(`t', `[$10][$1]')", "t(a,b,c,d,e,f,g,h,i,J)"),
            "[J][a]"
        );
    }

    #[test]
    fn test_unbalanced_parentheses_are_text() {
        assert_eq!(
            expand("", "((h1 eq h2 and l1 eq l2) or t1 == mlstrustedsubject);"),
            "((h1 eq h2 and l1 eq l2) or t1 == mlstrustedsubject);"
        );
        assert_eq!(expand("", "foo)"), "foo)");
    }

    #[test]
    fn test_comments_pass_through_unexpanded() {
        assert_eq!(
            expand("define(`foo', `X')", "a # c foo\nfoo"),
            "a # c foo\nX"
        );
    }

    #[test]
    fn test_bare_builtins_are_text() {
        assert_eq!(
            expand("", "define ifelse eval dnl\nnext"),
            "define ifelse eval next"
        );
    }

    #[test]
    fn test_arithmetic_builtins() {
        assert_eq!(
            expand(
                "define(`level', `202504')",
                "eval(level >= 202504) incr(4) decr(4) eval(255, 16, 4) len(`abc')"
            ),
            "1 5 3 00ff 3"
        );
    }

    #[test]
    fn test_shift_and_undefine() {
        assert_eq!(expand("", "shift(a, b, c)"), "b,c");
        assert_eq!(expand("define(`x', `1')undefine(`x')", "x"), "x");
    }

    #[test]
    fn test_deep_tail_recursion() {
        // AOSP's mls_macros generate categories this way, 1024 levels deep.
        let output = expand(
            "define(`count', `ifelse($1, 0, , `count(decr($1))c$1 ')')",
            "count(2000)",
        );
        assert!(output.starts_with("c1 c2 c3 "));
        assert!(output.ends_with("c1999 c2000 "));
    }

    #[test]
    fn test_error_max_depth() {
        let mut registry = MacroRegistry::new();
        registry.load("define(`loop', `loop')").unwrap();

        let mut expander = Expander::new(registry);
        let err = expander.expand("loop").unwrap_err();
        assert!(matches!(
            err,
            M4Error::MaxDepthExceeded(Expander::DEFAULT_MAX_DEPTH)
        ));
    }

    #[test]
    fn test_error_nesting_limit() {
        // Each expansion of `x` opens another call of `f` inside f's arguments.
        let mut registry = MacroRegistry::new();
        registry
            .load("define(`f', `$1')define(`x', `f(x)')")
            .unwrap();

        let mut expander = Expander::new(registry);
        let err = expander.expand("x").unwrap_err();
        assert!(matches!(
            err,
            M4Error::NestingLimitExceeded(Expander::MAX_NESTING)
        ));
    }

    #[test]
    fn test_error_unterminated_input() {
        let mut expander = Expander::new(MacroRegistry::new());
        assert!(matches!(
            expander.expand("`never closed").unwrap_err(),
            M4Error::UnterminatedQuote
        ));
        assert!(matches!(
            expander.expand("ifelse(a, b").unwrap_err(),
            M4Error::UnterminatedArguments(name) if name == "ifelse"
        ));
    }

    #[test]
    fn test_error_bad_numbers() {
        let mut expander = Expander::new(MacroRegistry::new());
        assert!(matches!(
            expander.expand("eval(1/0)").unwrap_err(),
            M4Error::Eval {
                reason: "divide by zero",
                ..
            }
        ));
        assert!(matches!(
            expander.expand("incr(abc)").unwrap_err(),
            M4Error::InvalidNumber {
                builtin: "incr",
                ..
            }
        ));
    }

    #[test]
    fn test_error_load_missing_file() {
        let mut registry = MacroRegistry::new();
        let err = registry.load_file("does/not/exist.m4").unwrap_err();
        assert!(matches!(&err, M4Error::Io { path, .. } if path == "does/not/exist.m4"));
    }
}
