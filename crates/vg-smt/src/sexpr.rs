//! SMT-LIB2 S-expression AST, deterministic printer, and streaming reader.

use std::fmt;
use std::io::{self, BufRead};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SExpr {
    Atom(String),
    Str(String),
    List(Vec<SExpr>),
}

impl SExpr {
    pub fn as_atom(&self) -> Option<&str> {
        match self {
            SExpr::Atom(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            SExpr::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[SExpr]> {
        match self {
            SExpr::List(xs) => Some(xs),
            _ => None,
        }
    }
}

impl fmt::Display for SExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SExpr::Atom(s) => f.write_str(s),
            SExpr::Str(s) => {
                f.write_str("\"")?;
                for ch in s.chars() {
                    if ch == '"' {
                        f.write_str("\"\"")?;
                    } else {
                        write!(f, "{ch}")?;
                    }
                }
                f.write_str("\"")
            }
            SExpr::List(xs) => {
                f.write_str("(")?;
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{x}")?;
                }
                f.write_str(")")
            }
        }
    }
}

fn next_byte<R: BufRead>(r: &mut R) -> io::Result<Option<u8>> {
    let buf = r.fill_buf()?;
    if buf.is_empty() {
        return Ok(None);
    }
    let b = buf[0];
    r.consume(1);
    Ok(Some(b))
}

fn peek_byte<R: BufRead>(r: &mut R) -> io::Result<Option<u8>> {
    let buf = r.fill_buf()?;
    Ok(buf.first().copied())
}

fn skip_ws_and_comments<R: BufRead>(r: &mut R) -> io::Result<()> {
    loop {
        match peek_byte(r)? {
            Some(b) if b.is_ascii_whitespace() => {
                next_byte(r)?;
            }
            Some(b';') => {
                while let Some(c) = next_byte(r)? {
                    if c == b'\n' {
                        break;
                    }
                }
            }
            _ => return Ok(()),
        }
    }
}

fn read_string<R: BufRead>(r: &mut R) -> io::Result<SExpr> {
    let mut bytes = Vec::new();
    loop {
        match next_byte(r)? {
            Some(b'"') => {
                if peek_byte(r)? == Some(b'"') {
                    next_byte(r)?;
                    bytes.push(b'"');
                } else {
                    let s = String::from_utf8(bytes)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    return Ok(SExpr::Str(s));
                }
            }
            Some(b) => bytes.push(b),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated SMT-LIB2 string literal",
                ));
            }
        }
    }
}

fn read_quoted_symbol<R: BufRead>(r: &mut R) -> io::Result<SExpr> {
    let mut bytes = vec![b'|'];
    loop {
        match next_byte(r)? {
            Some(b'|') => {
                bytes.push(b'|');
                let s = String::from_utf8(bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                return Ok(SExpr::Atom(s));
            }
            Some(b) => bytes.push(b),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated SMT-LIB2 quoted symbol",
                ));
            }
        }
    }
}

fn read_atom<R: BufRead>(r: &mut R, first: u8) -> io::Result<SExpr> {
    let mut bytes = vec![first];
    while let Some(b) = peek_byte(r)? {
        if b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b';' | b'"') {
            break;
        }
        next_byte(r)?;
        bytes.push(b);
    }
    let s = String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(SExpr::Atom(s))
}

/// Reads one balanced SMT-LIB2 S-expression from `r`, returning `None` on clean EOF.
pub fn read_sexpr<R: BufRead>(r: &mut R) -> io::Result<Option<SExpr>> {
    skip_ws_and_comments(r)?;
    let Some(first) = next_byte(r)? else {
        return Ok(None);
    };
    let mut stack: Vec<Vec<SExpr>> = Vec::new();
    match first {
        b'(' => stack.push(Vec::new()),
        b')' => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected ')' in SMT-LIB2 stream",
            ));
        }
        b'"' => return read_string(r).map(Some),
        b'|' => return read_quoted_symbol(r).map(Some),
        b => return read_atom(r, b).map(Some),
    }

    while !stack.is_empty() {
        skip_ws_and_comments(r)?;
        let Some(b) = next_byte(r)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected EOF inside SMT-LIB2 list",
            ));
        };
        let item = match b {
            b'(' => {
                stack.push(Vec::new());
                continue;
            }
            b')' => {
                let completed = SExpr::List(stack.pop().expect("non-empty stack"));
                if let Some(parent) = stack.last_mut() {
                    parent.push(completed);
                    continue;
                }
                return Ok(Some(completed));
            }
            b'"' => read_string(r)?,
            b'|' => read_quoted_symbol(r)?,
            other => read_atom(r, other)?,
        };
        if let Some(top) = stack.last_mut() {
            top.push(item);
        }
    }
    unreachable!("loop returns when outermost list closes")
}

/// Parses a single complete SMT-LIB2 S-expression from a string.
pub fn parse_sexpr(s: &str) -> Result<SExpr, String> {
    let mut cur = io::Cursor::new(s.as_bytes());
    let expr = read_sexpr(&mut cur)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "empty S-expression".to_string())?;
    skip_ws_and_comments(&mut cur).map_err(|e| e.to_string())?;
    if peek_byte(&mut cur).map_err(|e| e.to_string())?.is_some() {
        return Err("trailing tokens after S-expression".to_string());
    }
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_get_value_and_errors() {
        let input = "((x (fp #b0 #xfe #b10111101011111111100000))\n (knee (_ -zero 8 24))\n (msg \"a \"\"quoted\"\" err\"))\n";
        let e = parse_sexpr(input).unwrap();
        let list = e.as_list().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].as_list().unwrap()[0].as_atom(), Some("x"));
        assert_eq!(
            list[2].as_list().unwrap()[1].as_str(),
            Some("a \"quoted\" err")
        );
    }

    #[test]
    fn reads_sequential_responses_without_overreading() {
        let stream = b"; comment\nunsat\n(error \"bad\")\n";
        let mut cur = io::Cursor::new(&stream[..]);
        assert_eq!(
            read_sexpr(&mut cur).unwrap(),
            Some(SExpr::Atom("unsat".into()))
        );
        assert_eq!(
            read_sexpr(&mut cur).unwrap(),
            Some(SExpr::List(vec![
                SExpr::Atom("error".into()),
                SExpr::Str("bad".into()),
            ]))
        );
        assert_eq!(read_sexpr(&mut cur).unwrap(), None);
    }
}
