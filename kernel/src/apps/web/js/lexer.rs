//! JavaScript tokens.
//!
//! The awkward parts of JavaScript lexing are all here and none of them are
//! optional:
//!
//!   * a `/` is division or the start of a regular expression depending on
//!     what came before it, so the lexer has to remember the previous token;
//!   * automatic semicolon insertion means a newline is sometimes a statement
//!     terminator, so newlines cannot simply be skipped;
//!   * template literals nest, and `${}` inside one contains expressions that
//!     contain strings that contain more `${}`.
//!
//! Regular expressions are lexed but not supported — the token exists so that
//! `a / b` and `a.replace(/x/, 'y')` do not turn the rest of the file into
//! garbage, and the parser reports them as unsupported rather than silently
//! mis-reading them.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    // Literals and names.
    Number(f64),
    Str(Rc<str>),
    /// A template literal with no substitutions.
    Template(Rc<str>),
    Name(Rc<str>),
    /// `\`name` preceded by `...`.
    Regex,
    Keyword(&'static str),

    // Punctuation.
    Punct(&'static str),

    /// A newline was crossed before this token.  Kept because it is how a
    /// statement ends when there is no semicolon.
    Newline,

    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
}

const KEYWORDS: &[&str] = &[
    "var", "let", "const", "function", "return", "if", "else", "while", "for",
    "do", "break", "continue", "new", "delete", "typeof", "instanceof", "in",
    "this", "null", "true", "false", "undefined", "throw", "try", "catch",
    "finally", "switch", "case", "default", "void", "class", "extends",
    "super", "yield", "async", "await", "of", "static", "import", "export",
];

/// The longest punctuation first, so `>>>=` is not read as `>` `>` `>` `=`.
const PUNCT: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=",
    "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=",
    "*=", "/=", "%=", "&=", "|=", "^=", "**", "=>", "<<", ">>",
    "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/",
    "%", "&", "|", "^", "!", "~", "?", ":", "=", ".",
];

pub fn lex(src: &str) -> Result<Vec<Token>, (String, u32)> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;
    // Did the previous token allow a `/` to start a regular expression?
    let mut regex_ok = true;
    let mut saw_newline = false;

    while i < b.len() {
        let c = b[i];

        // Whitespace, and the newlines that matter.
        if c == '\n' {
            line += 1;
            saw_newline = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Comments.  A line comment is a newline for insertion purposes; a
        // block comment is only one if it contains one.
        if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    line += 1;
                    saw_newline = true;
                }
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }

        // A regular expression, only where one can start.
        if c == '/' && regex_ok {
            let start = i;
            i += 1;
            let mut in_class = false;
            while i < b.len() {
                match b[i] {
                    '\\' => i += 1,
                    '[' => in_class = true,
                    ']' => in_class = false,
                    '/' if !in_class => break,
                    '\n' => break,
                    _ => {}
                }
                i += 1;
            }
            if i < b.len() && b[i] == '/' {
                i += 1;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;             // flags
                }
                let _ = start;
                out.push(Token { tok: Tok::Regex, line });
                regex_ok = false;
                saw_newline = false;
                continue;
            }
            // Not a regex after all: put the `/` back.
            i = start;
        }

        if saw_newline {
            out.push(Token { tok: Tok::Newline, line });
            saw_newline = false;
        }

        // A number.
        if c.is_ascii_digit()
            || (c == '.' && i + 1 < b.len() && b[i + 1].is_ascii_digit())
        {
            let start = i;
            if c == '0' && i + 1 < b.len() && (b[i + 1] == 'x' || b[i + 1] == 'X') {
                i += 2;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = b[start + 2..i].iter().collect();
                let v = u64::from_str_radix(&text, 16).unwrap_or(0) as f64;
                out.push(Token { tok: Tok::Number(v), line });
            } else if c == '0' && i + 1 < b.len() && (b[i + 1] == 'b' || b[i + 1] == 'B') {
                i += 2;
                while i < b.len() && (b[i] == '0' || b[i] == '1') {
                    i += 1;
                }
                let text: String = b[start + 2..i].iter().collect();
                let v = u64::from_str_radix(&text, 2).unwrap_or(0) as f64;
                out.push(Token { tok: Tok::Number(v), line });
            } else {
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                    i += 1;
                }
                // An exponent, which is part of the number only if it has one.
                if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
                    let save = i;
                    i += 1;
                    if i < b.len() && (b[i] == '+' || b[i] == '-') {
                        i += 1;
                    }
                    if i < b.len() && b[i].is_ascii_digit() {
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    } else {
                        i = save;
                    }
                }
                let text: String = b[start..i].iter().collect();
                out.push(Token { tok: Tok::Number(text.parse().unwrap_or(0.0)), line });
            }
            regex_ok = false;
            continue;
        }

        // A string.
        if c == '"' || c == '\'' {
            let (s, next, nl) = read_string(&b, i, c)?;
            line += nl;
            i = next;
            out.push(Token { tok: Tok::Str(Rc::from(s.as_str())), line });
            regex_ok = false;
            continue;
        }

        // A template literal.  Substitutions are not supported and are
        // reported, rather than silently joined into the text.
        if c == '`' {
            let (s, next, nl) = read_string(&b, i, '`')?;
            line += nl;
            i = next;
            if s.contains("${") {
                return Err(("template substitutions are not supported".to_string(), line));
            }
            out.push(Token { tok: Tok::Template(Rc::from(s.as_str())), line });
            regex_ok = false;
            continue;
        }

        // A name or a keyword.
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < b.len()
                && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '$')
            {
                i += 1;
            }
            let text: String = b[start..i].iter().collect();
            let tok = match KEYWORDS.iter().find(|k| **k == text) {
                Some(k) => Tok::Keyword(k),
                None => Tok::Name(Rc::from(text.as_str())),
            };
            // After these, a `/` is division: `return a / b`.
            regex_ok = !matches!(tok, Tok::Keyword(_))
                || !matches!(
                    text.as_str(),
                    "return" | "typeof" | "delete" | "void" | "in" | "instanceof"
                        | "new" | "do" | "else" | "case"
                );
            if matches!(tok, Tok::Keyword("this") | Tok::Keyword("null")
                             | Tok::Keyword("true") | Tok::Keyword("false")
                             | Tok::Keyword("undefined")) {
                regex_ok = false;
            }
            out.push(Token { tok, line });
            continue;
        }

        // Punctuation, longest first.
        let rest: String = b[i..].iter().take(4).collect();
        match PUNCT.iter().find(|p| rest.starts_with(**p)) {
            Some(p) => {
                i += p.chars().count();
                out.push(Token { tok: Tok::Punct(p), line });
                // After a value or a `)`, a `/` can only be division.
                regex_ok = !matches!(*p, ")" | "]" | "}" | "++" | "--")
                    && *p != "this";
            }
            None => {
                return Err((
                    alloc::format!("unexpected character {:?}", c),
                    line,
                ));
            }
        }
    }

    out.push(Token { tok: Tok::Eof, line });
    Ok(out)
}

/// Read a quoted string, returning the value, the position after it and the
/// number of newlines crossed.
fn read_string(b: &[char], start: usize, quote: char) -> Result<(String, usize, u32), (String, u32)> {
    let mut i = start + 1;
    let mut out = String::new();
    let mut newlines = 0u32;
    while i < b.len() {
        let c = b[i];
        if c == quote {
            return Ok((out, i + 1, newlines));
        }
        if c == '\n' && quote != '`' {
            // An unterminated string ends at the line: that is what the
            // specification does, and it keeps one mistake from swallowing the
            // rest of the file.
            return Err(("unterminated string".to_string(), 0));
        }
        if c == '\n' {
            newlines += 1;
        }
        if c == '\\' && i + 1 < b.len() {
            let e = b[i + 1];
            i += 2;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'v' => out.push('\u{b}'),
                '0' => out.push('\0'),
                'x' => {
                    let hex: String = b[i..(i + 2).min(b.len())].iter().collect();
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        out.push(v as char);
                    }
                    i += 2;
                }
                'u' => {
                    let hex: String = b[i..(i + 4).min(b.len())].iter().collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        if let Some(c) = char::from_u32(v) {
                            out.push(c);
                        }
                    }
                    i += 4;
                }
                '\n' => newlines += 1,
                other => out.push(other),
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    Err(("unterminated string".to_string(), 0))
}
