//! Interpolated strings: `` `hp: {player.hp}/{max}` ``.
//!
//! The lexer keeps a whole interpolated string as one [`Token::InterpString`],
//! holes and all (a `{...}` hole can hold any expression, including more
//! strings and interpolations). The parser splits it up with [`pieces`] and
//! lexes each hole by itself.
//!
//! [`Token::InterpString`]: crate::Token::InterpString

use std::ops::Range;

/// One piece of an interpolated string, as byte ranges into its raw text
/// (backticks included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Literal text, still escaped.
    Text(Range<usize>),
    /// The inside of a `{...}` hole.
    Hole(Range<usize>),
}

/// Why an interpolated string couldn't be scanned, and how far the bad bit goes
/// (counting from just after the opening backtick).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// The string or a hole hit the end of the line or file.
    Unterminated(usize),
}

/// Scans the rest of an interpolated string after the opening backtick. Returns
/// how many bytes it covers, closing backtick included.
pub fn scan(rest: &[u8]) -> Result<usize, ScanError> {
    let mut i = 0;
    loop {
        match rest.get(i) {
            None | Some(b'\n') => return Err(ScanError::Unterminated(i)),
            Some(b'`') => return Ok(i + 1),
            Some(b'\\') => i += 2,
            Some(b'{') => i = scan_hole(rest, i + 1)?,
            Some(_) => i += 1,
        }
    }
}

/// Scans a hole's expression from `start` (just after the `{`), returns the
/// index just past its `}`.
fn scan_hole(rest: &[u8], start: usize) -> Result<usize, ScanError> {
    let mut depth = 0usize;
    let mut i = start;
    loop {
        match rest.get(i) {
            None => return Err(ScanError::Unterminated(i)),
            Some(b'{') => {
                depth += 1;
                i += 1;
            }
            Some(b'}') if depth == 0 => return Ok(i + 1),
            Some(b'}') => {
                depth -= 1;
                i += 1;
            }
            Some(&q @ (b'"' | b'\'')) => {
                i += 1;
                loop {
                    match rest.get(i) {
                        None | Some(b'\n') => return Err(ScanError::Unterminated(i)),
                        Some(b'\\') => i += 2,
                        Some(&c) if c == q => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            Some(b'`') => {
                i += 1 + scan(&rest[i + 1..])
                    .map_err(|ScanError::Unterminated(n)| ScanError::Unterminated(i + 1 + n))?
            }
            Some(b'[') => {
                let level = rest[i + 1..].iter().take_while(|&&b| b == b'=').count();
                if rest.get(i + 1 + level) == Some(&b'[') {
                    i += 2 + level;
                    i +=
                        long_close(&rest[i..], level).ok_or(ScanError::Unterminated(rest.len()))?;
                } else {
                    i += 1;
                }
            }
            Some(b'-') if rest.get(i + 1) == Some(&b'-') => {
                i += 2;
                let level = (rest.get(i) == Some(&b'['))
                    .then(|| rest[i + 1..].iter().take_while(|&&b| b == b'=').count())
                    .filter(|&l| rest.get(i + 1 + l) == Some(&b'['));
                match level {
                    Some(level) => {
                        i += 2 + level;
                        i += long_close(&rest[i..], level)
                            .ok_or(ScanError::Unterminated(rest.len()))?;
                    }
                    None => {
                        while rest.get(i).is_some_and(|&b| b != b'\n') {
                            i += 1;
                        }
                    }
                }
            }
            Some(_) => i += 1,
        }
    }
}

/// Bytes up to and including `]=*]` of the given level.
fn long_close(rest: &[u8], level: usize) -> Option<usize> {
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b']' {
            let eqs = rest[i + 1..].iter().take_while(|&&b| b == b'=').count();
            if eqs == level && rest.get(i + 1 + eqs) == Some(&b']') {
                return Some(i + eqs + 2);
            }
        }
        i += 1;
    }
    None
}

/// Splits an interpolated string token (as the lexer made it) into text and
/// holes. Empty text pieces get dropped.
pub fn pieces(raw: &str) -> Vec<Piece> {
    let bytes = raw.as_bytes();
    let end = bytes.len() - 1; // the closing backtick
    let mut out = Vec::new();
    let mut text_start = 1;
    let mut i = 1;
    while i < end {
        match bytes[i] {
            b'\\' => i += 2,
            b'{' => {
                if i > text_start {
                    out.push(Piece::Text(text_start..i));
                }
                let close = scan_hole(bytes, i + 1).expect("the lexer checked the hole") - 1;
                out.push(Piece::Hole(i + 1..close));
                i = close + 1;
                text_start = i;
            }
            _ => i += 1,
        }
    }
    if end > text_start {
        out.push(Piece::Text(text_start..end));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_nested_holes_strings_and_comments() {
        for (src, len) in [
            ("`plain`", 7),
            ("`a {b} c`", 9),
            ("`{ {1, 2} }`", 12),
            ("`{f('}')}`", 10),
            ("`{`in {x}`}` after", 12),
            ("`{a --[[ } ]] }`", 16),
            ("`\\{ not a hole \\}`", 18),
        ] {
            assert_eq!(scan(&src.as_bytes()[1..]).map(|n| n + 1), Ok(len), "{src}");
        }
        assert!(scan(b"unterminated").is_err());
        assert!(scan(b"{open").is_err());
        assert!(scan(b"line\nbreak`").is_err());
    }

    #[test]
    fn splits_into_pieces() {
        let raw = "`hp: {p.hp}/{max} ok`";
        let ps = pieces(raw);
        let text: Vec<&str> = ps
            .iter()
            .map(|p| match p {
                Piece::Text(r) | Piece::Hole(r) => &raw[r.clone()],
            })
            .collect();
        assert_eq!(text, ["hp: ", "p.hp", "/", "max", " ok"]);
        assert!(matches!(ps[1], Piece::Hole(_)));
    }
}
