//! Turns string literal tokens into their runtime text.
//!
//! Rhue strings are UTF-8 (`str`), not byte strings like Lua's. Escapes that
//! would make invalid UTF-8 (`"\xff"`) get rejected at compile time.

use rhue_common::{Diagnostic, Span};

/// Decodes a quoted (`"..."`, `'...'`) or long (`[[...]]`) string token.
#[allow(clippy::result_large_err)] // cold path: one call per string literal
pub fn decode(raw: &str, span: Span) -> Result<String, Diagnostic> {
    if raw.starts_with('[') {
        return Ok(decode_long(raw));
    }
    decode_quoted(raw, span)
}

fn decode_long(raw: &str) -> String {
    let level = raw[1..].bytes().take_while(|&b| b == b'=').count();
    let body = &raw[level + 2..raw.len() - level - 2];
    // a newline right after the opening bracket gets skipped
    let body = body
        .strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body);
    body.to_owned()
}

#[allow(clippy::result_large_err)]
fn decode_quoted(raw: &str, span: Span) -> Result<String, Diagnostic> {
    decode_escaped(&raw[1..raw.len() - 1], span.start + 1, false)
}

/// Decodes a text piece of an interpolated string starting at byte `start` of
/// the source. `` \` ``, `\{` and `\}` are escapes in there too.
#[allow(clippy::result_large_err)]
pub fn decode_interp_text(body: &str, start: u32) -> Result<String, Diagnostic> {
    decode_escaped(body, start, true)
}

#[allow(clippy::result_large_err)]
fn decode_escaped(body: &str, base: u32, interp: bool) -> Result<String, Diagnostic> {
    let mut bytes: Vec<u8> = Vec::with_capacity(body.len());
    let src = body.as_bytes();
    let err = |at: usize, len: usize, msg: &str| {
        let s = base + at as u32;
        Diagnostic::error(msg.to_string(), Span::new(s, s + len as u32))
    };

    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b != b'\\' {
            bytes.push(b);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let Some(&e) = src.get(i) else {
            return Err(err(start, 1, "unfinished escape sequence"));
        };
        i += 1;
        match e {
            b'a' => bytes.push(0x07),
            b'b' => bytes.push(0x08),
            b'f' => bytes.push(0x0c),
            b'n' => bytes.push(b'\n'),
            b'r' => bytes.push(b'\r'),
            b't' => bytes.push(b'\t'),
            b'v' => bytes.push(0x0b),
            b'\\' => bytes.push(b'\\'),
            b'"' => bytes.push(b'"'),
            b'\'' => bytes.push(b'\''),
            b'`' | b'{' | b'}' if interp => bytes.push(e),
            b'\n' => bytes.push(b'\n'),
            b'\r' => {
                bytes.push(b'\n');
                if src.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'z' => {
                while src.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                    i += 1;
                }
            }
            b'x' => {
                let hex = src
                    .get(i..i + 2)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
                let Some(hex) = hex else {
                    return Err(err(start, 2, "`\\x` must be followed by two hex digits"));
                };
                bytes.push(u8::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap());
                i += 2;
            }
            b'u' => {
                if src.get(i) != Some(&b'{') {
                    return Err(err(start, 2, "`\\u` must be followed by `{`"));
                }
                let close = src[i..].iter().position(|&c| c == b'}').map(|p| i + p);
                let Some(close) = close else {
                    return Err(err(start, i - start + 1, "unterminated `\\u{...}` escape"));
                };
                let digits = std::str::from_utf8(&src[i + 1..close]).unwrap_or("");
                let ch = u32::from_str_radix(digits, 16)
                    .ok()
                    .and_then(char::from_u32);
                let Some(ch) = ch else {
                    return Err(err(start, close + 1 - start, "invalid unicode code point"));
                };
                let mut buf = [0; 4];
                bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i = close + 1;
            }
            b'0'..=b'9' => {
                let mut value = (e - b'0') as u32;
                let mut n = 1;
                while n < 3 && src.get(i).is_some_and(u8::is_ascii_digit) {
                    value = value * 10 + (src[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                if value > 255 {
                    return Err(err(start, i - start, "decimal escape is larger than 255"));
                }
                bytes.push(value as u8);
            }
            _ => return Err(err(start, 2, "invalid escape sequence")),
        }
    }

    String::from_utf8(bytes).map_err(|_| {
        Diagnostic::error(
            "string is not valid UTF-8",
            Span::new(base - 1, base + body.len() as u32 + 1),
        )
        .with_label("an escape in this string produces invalid UTF-8")
        .with_note("Rhue strings are UTF-8; use `\\u{...}` for non-ASCII characters")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(raw: &str) -> String {
        decode(raw, Span::new(0, raw.len() as u32)).unwrap()
    }

    fn e(raw: &str) -> String {
        decode(raw, Span::new(0, raw.len() as u32))
            .unwrap_err()
            .message
    }

    #[test]
    fn escapes() {
        assert_eq!(d(r#""a\tb\n""#), "a\tb\n");
        assert_eq!(d(r#"'it\'s'"#), "it's");
        assert_eq!(d(r#""\65\066\x43""#), "ABC");
        assert_eq!(d(r#""\u{48}\u{e9}\u{1F600}""#), "Hé😀");
        assert_eq!(d("\"a\\z   \n   b\""), "ab");
        assert_eq!(d("\"line\\\nnext\""), "line\nnext");
    }

    #[test]
    fn long_strings() {
        assert_eq!(d("[[hello]]"), "hello");
        assert_eq!(
            d("[==[\nfirst newline skipped]]still]==]"),
            "first newline skipped]]still"
        );
        assert_eq!(d("[[no \\n escapes]]"), "no \\n escapes");
    }

    #[test]
    fn errors() {
        assert_eq!(e(r#""\q""#), "invalid escape sequence");
        assert_eq!(e(r#""\x4""#), "`\\x` must be followed by two hex digits");
        assert_eq!(e(r#""\300""#), "decimal escape is larger than 255");
        assert_eq!(e(r#""\xff""#), "string is not valid UTF-8");
        assert_eq!(e(r#""\u{110000}""#), "invalid unicode code point");
    }
}
