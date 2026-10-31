//! A streaming M4 macro processor following GNU m4.
//!
//! Input is read through a stack of pushed-back strings. A macro's expansion
//! is pushed back and rescanned, so it can produce further macro calls or
//! complete a call whose arguments continue in the remaining input. Input is
//! split into words (`[A-Za-z_][A-Za-z0-9_]*`), quoted strings, comments and
//! single characters, so a macro name is recognized wherever it appears, for
//! example after `:` or `-`.

use std::collections::HashMap;
use std::io::{self, Read, Write};

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
        let registry = std::mem::take(self);
        let mut scanner = Scanner::new(registry, source.as_bytes(), Expander::DEFAULT_MAX_DEPTH);
        let result = scanner.run(&mut io::sink());
        *self = scanner.into_registry();
        result
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
        let mut out = Vec::with_capacity(input.len());
        self.expand_to(input.as_bytes(), &mut out)?;
        Ok(String::from_utf8(out).expect("expanding UTF-8 input produces UTF-8"))
    }

    /// Expand UTF-8 input from `reader` into `writer` as it is read. Only the
    /// text of a pending macro call, quoted string or comment is held in
    /// memory; other output is written as soon as it is complete.
    pub fn expand_to(&mut self, reader: impl Read, mut writer: impl Write) -> Result<(), M4Error> {
        let registry = std::mem::take(&mut self.registry);
        let mut scanner = Scanner::new(registry, reader, self.max_depth);
        let result = scanner.run(&mut writer);
        self.registry = scanner.into_registry();
        result
    }
}

/// Input not yet read from the underlying reader, decoded one chunk at a
/// time. Consumed text is dropped when the next chunk is decoded.
struct Source<R> {
    reader: R,
    text: String,
    pos: usize,
    /// Bytes of a character split across chunks.
    partial: Vec<u8>,
    eof: bool,
}

const CHUNK: usize = 8192;

impl<R: Read> Source<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            text: String::new(),
            pos: 0,
            partial: Vec::new(),
            eof: false,
        }
    }

    /// Decode the next chunk; `false` once the reader is exhausted. A chunk
    /// may decode to nothing when it ends inside a multi-byte character.
    fn fill(&mut self) -> Result<bool, M4Error> {
        if self.eof {
            return Ok(false);
        }
        let mut chunk = [0u8; CHUNK];
        let read = loop {
            match self.reader.read(&mut chunk) {
                Ok(read) => break read,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(M4Error::Read(err)),
            }
        };
        if read == 0 {
            self.eof = true;
            if !self.partial.is_empty() {
                return Err(M4Error::InvalidUtf8);
            }
            return Ok(false);
        }

        self.partial.extend_from_slice(&chunk[..read]);
        let valid = match std::str::from_utf8(&self.partial) {
            Ok(_) => self.partial.len(),
            Err(err) if err.error_len().is_none() => err.valid_up_to(),
            Err(_) => return Err(M4Error::InvalidUtf8),
        };
        self.text.clear();
        self.text
            .push_str(std::str::from_utf8(&self.partial[..valid]).expect("validated as UTF-8"));
        self.partial.drain(..valid);
        self.pos = 0;
        Ok(true)
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

/// Expansion state: pushed-back expansions over the remaining input, and
/// the calls still collecting arguments. Calls live on this explicit stack
/// rather than the Rust call stack, so nesting is bounded by
/// [`Expander::MAX_NESTING`], not by the thread's stack size.
struct Scanner<R> {
    registry: MacroRegistry,
    frames: Vec<Frame>,
    source: Source<R>,
    calls: Vec<PendingCall>,
    /// GNU m4 skips unquoted whitespace at the start of each argument.
    at_arg_start: bool,
    max_depth: usize,
}

impl<R: Read> Scanner<R> {
    fn new(registry: MacroRegistry, reader: R, max_depth: usize) -> Self {
        Self {
            registry,
            frames: Vec::new(),
            source: Source::new(reader),
            calls: Vec::new(),
            at_arg_start: false,
            max_depth,
        }
    }

    fn into_registry(self) -> MacroRegistry {
        self.registry
    }

    /// Expand to the end of input, writing output as it accumulates.
    fn run(&mut self, writer: &mut impl Write) -> Result<(), M4Error> {
        let mut out = String::new();
        while self.step(&mut out)? {
            if out.len() >= CHUNK {
                writer.write_all(out.as_bytes()).map_err(M4Error::Write)?;
                out.clear();
            }
        }
        writer.write_all(out.as_bytes()).map_err(M4Error::Write)?;
        writer.flush().map_err(M4Error::Write)
    }

    /// Process one token, appending any finished output to `out`; `false`
    /// at the end of input.
    fn step(&mut self, out: &mut String) -> Result<bool, M4Error> {
        if self.at_arg_start {
            while self.peek()?.is_some_and(char::is_whitespace) {
                self.next_char()?;
            }
            self.at_arg_start = false;
        }

        match self.next_token()? {
            Token::Eof => {
                return match self.calls.pop() {
                    Some(call) => Err(M4Error::UnterminatedArguments(call.name)),
                    None => Ok(false),
                };
            }
            Token::Word(word, depth) => match self.callable(&word)? {
                Some(found) if self.peek()? == Some('(') => {
                    self.next_char()?;
                    if self.calls.len() == Expander::MAX_NESTING {
                        return Err(M4Error::NestingLimitExceeded(Expander::MAX_NESTING));
                    }
                    self.calls.push(PendingCall {
                        name: word,
                        found,
                        depth,
                        args: Vec::new(),
                        current: String::new(),
                        parens: 0,
                    });
                    self.at_arg_start = true;
                }
                Some(found) => {
                    let expansion = self.apply(&word, found, &[])?;
                    self.push(expansion, depth + 1)?;
                }
                None => sink(out, &mut self.calls).push_str(&word),
            },
            Token::Quoted(text) | Token::Comment(text) => {
                sink(out, &mut self.calls).push_str(&text)
            }
            Token::Char(c) => {
                let Some(call) = self.calls.last_mut() else {
                    out.push(c);
                    return Ok(true);
                };
                match c {
                    '(' => {
                        call.parens += 1;
                        call.current.push(c);
                    }
                    ')' if call.parens == 0 => {
                        let call = self.calls.pop().expect("a call is pending");
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
                        self.at_arg_start = true;
                    }
                    _ => call.current.push(c),
                }
            }
        }
        Ok(true)
    }

    /// Drop exhausted frames and return the next character without
    /// consuming it, reading more input when the frames run out.
    fn peek(&mut self) -> Result<Option<char>, M4Error> {
        while let Some(frame) = self.frames.last() {
            if let Some(c) = frame.text[frame.pos..].chars().next() {
                return Ok(Some(c));
            }
            self.frames.pop();
        }
        loop {
            if let Some(c) = self.source.text[self.source.pos..].chars().next() {
                return Ok(Some(c));
            }
            if !self.source.fill()? {
                return Ok(None);
            }
        }
    }

    fn next_char(&mut self) -> Result<Option<char>, M4Error> {
        let Some(c) = self.peek()? else {
            return Ok(None);
        };
        match self.frames.last_mut() {
            Some(frame) => frame.pos += c.len_utf8(),
            None => self.source.pos += c.len_utf8(),
        }
        Ok(Some(c))
    }

    fn push(&mut self, text: String, depth: usize) -> Result<(), M4Error> {
        if depth > self.max_depth {
            return Err(M4Error::MaxDepthExceeded(self.max_depth));
        }
        if !text.is_empty() {
            self.frames.push(Frame {
                text,
                pos: 0,
                depth,
            });
        }
        Ok(())
    }

    fn next_token(&mut self) -> Result<Token, M4Error> {
        let Some(c) = self.peek()? else {
            return Ok(Token::Eof);
        };

        if is_word_start(c) {
            let depth = self.frames.last().map_or(0, |frame| frame.depth);
            let mut word = String::new();
            while let Some(c) = self.peek()?.filter(|&c| is_word_char(c)) {
                word.push(c);
                self.next_char()?;
            }
            return Ok(Token::Word(word, depth));
        }

        self.next_char()?;
        match c {
            '`' => {
                let mut text = String::new();
                let mut level = 1;
                loop {
                    let c = self.next_char()?.ok_or(M4Error::UnterminatedQuote)?;
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
                while let Some(c) = self.next_char()? {
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
    fn callable(&mut self, word: &str) -> Result<Option<Macro>, M4Error> {
        let Some(found) = self.registry.0.get(word).cloned() else {
            return Ok(None);
        };
        if let Macro::Builtin(builtin) = found
            && builtin.needs_args()
            && self.peek()? != Some('(')
        {
            return Ok(None);
        }
        Ok(Some(found))
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
                while self.next_char()?.is_some_and(|c| c != '\n') {}
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

/// A reader that expands M4 input from another reader as it is read. Each
/// `read` expands only as much input as it needs to return some output.
pub struct ExpandingReader<R: Read> {
    scanner: Scanner<R>,
    out: String,
    out_pos: usize,
    done: bool,
}

impl<R: Read> ExpandingReader<R> {
    pub fn new(inner: R, registry: MacroRegistry) -> Self {
        Self {
            scanner: Scanner::new(registry, inner, Expander::DEFAULT_MAX_DEPTH),
            out: String::new(),
            out_pos: 0,
            done: false,
        }
    }

    /// The macro definitions, including any the input has defined so far.
    pub fn into_registry(self) -> MacroRegistry {
        self.scanner.into_registry()
    }
}

impl<R: Read> Read for ExpandingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.out_pos == self.out.len() && !self.done {
            self.out.clear();
            self.out_pos = 0;
            match self.scanner.step(&mut self.out) {
                Ok(true) => {}
                Ok(false) => self.done = true,
                Err(M4Error::Read(err)) => return Err(err),
                Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
            }
        }

        let available = &self.out.as_bytes()[self.out_pos..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        self.out_pos += len;
        Ok(len)
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
    fn test_quoted_string() {
        // Quoted text is not expanded; one level of quotes is removed.
        assert_eq!(expand("define(`foo', `bar')", "`foo' ``foo''"), "foo `foo'");
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

    /// Yields one byte per `read` call.
    struct ByteReader<'a>(&'a [u8]);

    impl Read for ByteReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.split_first() {
                Some((&byte, rest)) if !buf.is_empty() => {
                    buf[0] = byte;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    /// Repeats a pattern forever.
    struct Endless(&'static [u8], usize);

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            for byte in buf.iter_mut() {
                *byte = self.0[self.1 % self.0.len()];
                self.1 += 1;
            }
            Ok(buf.len())
        }
    }

    /// Accepts `limit` bytes, then fails.
    struct LimitedWriter {
        written: usize,
        limit: usize,
    }

    impl Write for LimitedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.written >= self.limit {
                return Err(io::Error::other("writer full"));
            }
            self.written += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn registry(macros: &str) -> MacroRegistry {
        let mut registry = MacroRegistry::new();
        registry.load(macros).unwrap();
        registry
    }

    #[test]
    fn test_streaming_matches_in_memory_expansion() {
        let macros = "define(`grüß', `$1 → ünïcode')define(`dir', `/données/$1')";
        let input = "a grüß(wörld) b dir(é) `quoted grüß' # comment grüß\nend";
        let expected = expand(macros, input);

        // One byte per read splits multi-byte characters and macro calls.
        let mut out = Vec::new();
        Expander::new(registry(macros))
            .expand_to(ByteReader(input.as_bytes()), &mut out)
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), expected);

        let mut reader = ExpandingReader::new(ByteReader(input.as_bytes()), registry(macros));
        let mut via_reader = String::new();
        reader.read_to_string(&mut via_reader).unwrap();
        assert_eq!(via_reader, expected);
    }

    #[test]
    fn test_expanding_reader_reads_incrementally() {
        // Buffering the whole input first would never return on endless input.
        let mut reader =
            ExpandingReader::new(Endless(b"foo ", 0), registry("define(`foo', `bar')"));
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"bar bar bar bar ");
    }

    #[test]
    fn test_expand_to_writes_while_reading() {
        let mut writer = LimitedWriter {
            written: 0,
            limit: 1 << 16,
        };
        let err = Expander::new(registry("define(`foo', `bar')"))
            .expand_to(Endless(b"foo ", 0), &mut writer)
            .unwrap_err();
        assert!(matches!(err, M4Error::Write(_)));
        assert!(writer.written >= 1 << 16);
    }

    #[test]
    fn test_error_invalid_utf8() {
        let mut expander = Expander::new(MacroRegistry::new());
        assert!(matches!(
            expander.expand_to(&b"ok \xff"[..], io::sink()).unwrap_err(),
            M4Error::InvalidUtf8
        ));
        // A multi-byte character cut off at the end of input.
        assert!(matches!(
            expander
                .expand_to(&"é".as_bytes()[..1], io::sink())
                .unwrap_err(),
            M4Error::InvalidUtf8
        ));
    }
}
