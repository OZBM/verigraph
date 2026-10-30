//! The VG-Sym lexer (docs/03, "Lexical rules"). It reproduces tools/vgsym.lark's basic lexer token
//! for token: a word that the grammar spells is a keyword; mixed-case keywords such as `Bool` are
//! separate terminals, so, as in Lark, they also match as the prefix of a longer word.

use crate::diag::{Diagnostic, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    OpName(String),
    /// A reserved word, `@MESH_QUERY` included.
    Kw(&'static str),
    /// A port, without its `%`.
    Port(String),
    /// A label, without its `@`.
    Label(String),
    Int(String),
    Decimal(String),
    /// A string literal as written, quotes included.
    Str(String),
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub const LOWER_KEYWORDS: &[&str] = &[
    "module",
    "signature",
    "effects",
    "pre",
    "post",
    "intent",
    "export",
    "inline",
    "in",
    "loss",
    "true",
    "false",
    "u8",
    "u16",
    "u32",
    "u64",
    "i8",
    "i16",
    "i32",
    "i64",
    "f16",
    "bf16",
    "f32",
    "f64",
    "result",
    "forall",
    "spec",
    "fold",
];

pub const UPPER_KEYWORDS: &[&str] = &[
    "SPEC",
    "NODE",
    "SIGNATURE",
    "EFFECTS",
    "CONTRACT",
    "BODY",
    "PRE",
    "POST",
    "INVARIANT",
    "DECREASES",
    "COST",
    "BUDGET",
    "LEARNABLE",
    "RETURN",
    "ARENA",
    "CLOCK",
    "FILE_READ",
    "FILE_WRITE",
    "NET_READ",
    "NET_WRITE",
    "GPU",
    "NONDET",
    "PARAM_UPDATE",
    "TELEMETRY",
    "HOST_CALL",
    "SPAWN",
];

/// Longest first, the order in which Lark tries these string terminals.
const MIXED_KEYWORDS: &[&str] = &[
    "GpuQueue", "FileOut", "FileIn", "Tensor", "Socket", "Result", "Array", "Arena", "Slice",
    "Tuple", "Clock", "Bool", "Eff",
];

/// Longest first.
const PUNCT: &[&str] = &[
    "==>", "==", "!=", "<=", ">=", "&&", "||", "->", "..", ";", "{", "}", "(", ")", "[", "]", "<",
    ">", ",", ":", "=", "|", "!", "-", "+", "*", "/", "%", ".",
];

pub fn is_keyword(word: &str) -> bool {
    keyword(word).is_some()
}

fn keyword(word: &str) -> Option<&'static str> {
    LOWER_KEYWORDS
        .iter()
        .chain(UPPER_KEYWORDS)
        .chain(MIXED_KEYWORDS)
        .find(|k| **k == word)
        .copied()
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Lex a whole source. Whitespace (space, tab, form feed, CR, LF) and `//` comments separate tokens.
pub fn lex(src: &str) -> Result<Vec<Token>, Diagnostic> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);
    while i < s.len() {
        let c = s[i];
        let start = (i, line, col);
        let mut tok_end = i + 1;
        let tok = match c {
            ' ' | '\t' | '\x0c' | '\r' | '\n' => None,
            '/' if s.get(i + 1) == Some(&'/') => {
                while tok_end < s.len() && s[tok_end] != '\n' {
                    tok_end += 1;
                }
                None
            }
            '"' => {
                let mut j = i + 1;
                loop {
                    match s.get(j) {
                        None => {
                            return Err(err(
                                "VG-P0002",
                                start,
                                1,
                                "unterminated string: a string ends with \"",
                            ));
                        }
                        Some('"') => break,
                        // An escape is a backslash and any character but a newline, as in Lark.
                        Some('\\') if matches!(s.get(j + 1), Some(c) if *c != '\n') => j += 2,
                        Some('\\') => {
                            return Err(err(
                                "VG-P0002",
                                start,
                                1,
                                "unterminated string: a backslash must be followed by a character",
                            ));
                        }
                        Some(_) => j += 1,
                    }
                }
                tok_end = j + 1;
                Some(Tok::Str(s[i..tok_end].iter().collect()))
            }
            '%' if s.get(i + 1).is_some_and(char::is_ascii_digit) => {
                tok_end = digits(&s, i + 1);
                Some(Tok::Port(s[i + 1..tok_end].iter().collect()))
            }
            '@' => {
                if s.get(i + 1)
                    .is_some_and(|c| c.is_ascii_lowercase() || *c == '_')
                {
                    tok_end = word_end(&s, i + 1);
                    Some(Tok::Label(s[i + 1..tok_end].iter().collect()))
                } else if starts_with(&s, i, "@MESH_QUERY") {
                    tok_end = i + 11;
                    Some(Tok::Kw("@MESH_QUERY"))
                } else {
                    return Err(err(
                        "VG-P0005",
                        start,
                        1,
                        "`@` starts a label (`@name`, lowercase first) or `@MESH_QUERY`",
                    ));
                }
            }
            '0'..='9' => {
                let (end, decimal) = number(&s, i);
                tok_end = end;
                let text: String = s[i..end].iter().collect();
                Some(if decimal {
                    Tok::Decimal(text)
                } else {
                    Tok::Int(text)
                })
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let end = word_end(&s, i);
                let word: String = s[i..end].iter().collect();
                if c.is_ascii_lowercase() || c == '_' {
                    tok_end = end;
                    Some(keyword(&word).map_or(Tok::Ident(word), Tok::Kw))
                } else if word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    tok_end = end;
                    Some(keyword(&word).map_or(Tok::OpName(word), Tok::Kw))
                } else if let Some(k) = MIXED_KEYWORDS.iter().find(|k| word.starts_with(**k)) {
                    tok_end = i + k.len();
                    Some(Tok::Kw(k))
                } else {
                    return Err(err(
                        "VG-P0005",
                        start,
                        (end - i) as u32,
                        format!(
                            "`{word}` is not a word of VG-Sym: names start lowercase, op names are uppercase"
                        ),
                    ));
                }
            }
            _ => match PUNCT.iter().find(|p| starts_with(&s, i, p)) {
                Some(p) => {
                    tok_end = i + p.len();
                    Some(Tok::Punct(p))
                }
                None => {
                    return Err(err(
                        "VG-P0005",
                        start,
                        1,
                        format!("no token starts with {c:?}"),
                    ));
                }
            },
        };
        if let Some(tok) = tok {
            out.push(Token {
                tok,
                span: Span {
                    line: start.1,
                    col: start.2,
                    len: (tok_end - i) as u32,
                    offset: i as u32,
                },
            });
        }
        for &ch in &s[i..tok_end] {
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        i = tok_end;
    }
    out.push(Token {
        tok: Tok::Eof,
        span: Span {
            line,
            col,
            len: 0,
            offset: s.len() as u32,
        },
    });
    Ok(out)
}

fn err(
    code: &'static str,
    (i, line, col): (usize, u32, u32),
    len: u32,
    msg: impl Into<String>,
) -> Diagnostic {
    Diagnostic::error(
        code,
        Span {
            line,
            col,
            len,
            offset: i as u32,
        },
        msg,
    )
}

fn starts_with(s: &[char], i: usize, p: &str) -> bool {
    p.chars().enumerate().all(|(k, c)| s.get(i + k) == Some(&c))
}

fn digits(s: &[char], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    i
}

fn word_end(s: &[char], mut i: usize) -> usize {
    while i < s.len() && is_word(s[i]) {
        i += 1;
    }
    i
}

/// `Decimal ::= [0-9]+ ("." [0-9]+ ([eE] [+-]? [0-9]+)? | [eE] [+-]? [0-9]+)`, tried before `Int`.
fn number(s: &[char], i: usize) -> (usize, bool) {
    let int_end = digits(s, i);
    let exponent = |j: usize| -> Option<usize> {
        if !matches!(s.get(j), Some('e' | 'E')) {
            return None;
        }
        let k = if matches!(s.get(j + 1), Some('+' | '-')) {
            j + 2
        } else {
            j + 1
        };
        let end = digits(s, k);
        (end > k).then_some(end)
    };
    if s.get(int_end) == Some(&'.') && s.get(int_end + 1).is_some_and(char::is_ascii_digit) {
        let frac_end = digits(s, int_end + 1);
        return (exponent(frac_end).unwrap_or(frac_end), true);
    }
    match exponent(int_end) {
        Some(end) => (end, true),
        None => (int_end, false),
    }
}

/// The text of each token, in order, as Lark's `Lark.lex` yields them; the parity check of docs/16
/// compares these lists.
pub fn token_texts(src: &str) -> Result<Vec<String>, Diagnostic> {
    let chars: Vec<char> = src.chars().collect();
    Ok(lex(src)?
        .iter()
        .filter(|t| t.tok != Tok::Eof)
        .map(|t| {
            let o = t.span.offset as usize;
            chars[o..o + t.span.len as usize].iter().collect()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<String> {
        token_texts(src).unwrap()
    }

    #[test]
    fn lexical_rules_of_docs_03() {
        assert_eq!(texts("x-1"), ["x", "-", "1"]);
        assert_eq!(texts("x%2"), ["x", "%2"]);
        assert_eq!(texts("x % 2"), ["x", "%", "2"]);
        assert_eq!(texts("t.0.1"), ["t", ".", "0.1"]);
        assert_eq!(texts("0..512"), ["0", "..", "512"]);
        assert_eq!(texts("in0..2"), ["in0", "..", "2"]);
        assert_eq!(
            texts("1.5e3 1e-7 1.5e 2E+2"),
            ["1.5e3", "1e-7", "1.5", "e", "2E+2"]
        );
        assert_eq!(texts("a==>b==c"), ["a", "==>", "b", "==", "c"]);
        assert_eq!(
            texts("@soft_knee %12 // gone\n@MESH_QUERY"),
            ["@soft_knee", "%12", "@MESH_QUERY"]
        );
        assert_eq!(texts(r#""a\"b\\" x"#), [r#""a\"b\\""#, "x"]);
    }

    #[test]
    fn keywords_and_lark_quirks() {
        let toks = lex("u8 u8x NODE NODEX Bool Boolx FileOut FileInx").unwrap();
        let kinds: Vec<_> = toks.iter().map(|t| t.tok.clone()).collect();
        assert_eq!(
            kinds,
            [
                Tok::Kw("u8"),
                Tok::Ident("u8x".into()),
                Tok::Kw("NODE"),
                Tok::OpName("NODEX".into()),
                Tok::Kw("Bool"),
                Tok::Kw("Bool"),
                Tok::Ident("x".into()),
                Tok::Kw("FileOut"),
                Tok::Kw("FileIn"),
                Tok::Ident("x".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexical_errors() {
        assert_eq!(lex("\"open").unwrap_err().code, "VG-P0002");
        assert_eq!(lex("a # b").unwrap_err().code, "VG-P0005");
        assert_eq!(lex("Foo").unwrap_err().code, "VG-P0005");
        assert_eq!(lex("@Soft").unwrap_err().code, "VG-P0005");
        assert_eq!(lex("x\u{a0}y").unwrap_err().code, "VG-P0005");
        let e = lex("ok\n  &").unwrap_err();
        assert_eq!((e.span.line, e.span.col), (2, 3));
    }
}
