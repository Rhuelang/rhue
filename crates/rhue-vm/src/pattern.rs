//! Lua 5.4 patterns, ported from `lstrlib.c`: character classes (`%a %c %d %g
//! %l %p %s %u %w %x`, upper case for the complement), sets (`[a-z%d]`,
//! `[^...]`), `.`, the quantifiers `* + - ?`, anchors, captures (position
//! captures `()` too), back-references `%1`-`%9`, `%bxy` and the frontier
//! `%f[set]`.
//!
//! Matching works on bytes like Lua's, so classes only know ASCII and `.`
//! matches one byte of a multi-byte character. The matcher backtracks
//! recursively with a depth limit ("pattern too complex"), same as Lua's.

/// How many captures a pattern can have.
pub const MAX_CAPTURES: usize = 32;
/// How deep the matcher can recurse.
const MAX_DEPTH: usize = 200;

const ESC: u8 = b'%';
const SPECIALS: &[u8] = b"^$*+?.([%-";

/// A capture: a byte range of the subject, or a position (`()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    Str(usize, usize),
    /// 0-based byte offset (scripts see it 1-based).
    Pos(usize),
}

#[derive(Clone, Copy)]
enum Len {
    Unfinished,
    Position,
    Closed(usize),
}

pub struct Matcher<'a> {
    src: &'a [u8],
    pat: &'a [u8],
    depth: usize,
    level: usize,
    caps: [(usize, Len); MAX_CAPTURES],
}

type M = Result<Option<usize>, String>;

/// Whether `pat` needs the pattern matcher (has a special character).
pub fn has_specials(pat: &[u8]) -> bool {
    pat.iter().any(|c| SPECIALS.contains(c))
}

impl<'a> Matcher<'a> {
    pub fn new(src: &'a [u8], pat: &'a [u8]) -> Self {
        Matcher {
            src,
            pat,
            depth: MAX_DEPTH,
            level: 0,
            caps: [(0, Len::Unfinished); MAX_CAPTURES],
        }
    }

    /// Tries to match the pattern from its byte `p` against the subject at byte
    /// `s`. Gives the end of the match, if there is one.
    pub fn try_at(&mut self, s: usize, p: usize) -> M {
        self.level = 0;
        self.depth = MAX_DEPTH;
        self.do_match(s, p)
    }

    /// The pattern's own captures from the last match (none if it has no
    /// parentheses).
    pub fn explicit_captures(&self) -> Result<Vec<Capture>, String> {
        (0..self.level).map(|i| self.capture(i)).collect()
    }

    /// The captures of the last match from `s` to `e`, or the whole match if
    /// the pattern has none.
    pub fn captures(&self, s: usize, e: usize) -> Result<Vec<Capture>, String> {
        if self.level == 0 {
            return Ok(vec![Capture::Str(s, e)]);
        }
        (0..self.level).map(|i| self.capture(i)).collect()
    }

    /// The first capture (or the whole match), what `gsub` indexes a
    /// replacement table with.
    pub fn first_capture(&self, s: usize, e: usize) -> Result<Capture, String> {
        if self.level == 0 {
            Ok(Capture::Str(s, e))
        } else {
            self.capture(0)
        }
    }

    /// Capture `i` (0-based). `%0` in a replacement is the whole match.
    pub fn capture_or_whole(&self, i: usize, s: usize, e: usize) -> Result<Capture, String> {
        if i == 0 && self.level == 0 {
            return Ok(Capture::Str(s, e));
        }
        if i >= self.level {
            return Err(format!("invalid capture index %{}", i + 1));
        }
        self.capture(i)
    }

    fn capture(&self, i: usize) -> Result<Capture, String> {
        let (init, len) = self.caps[i];
        match len {
            Len::Unfinished => Err("unfinished capture".into()),
            Len::Position => Ok(Capture::Pos(init)),
            Len::Closed(n) => Ok(Capture::Str(init, init + n)),
        }
    }

    fn pat_at(&self, p: usize) -> u8 {
        self.pat.get(p).copied().unwrap_or(0)
    }

    fn src_at(&self, s: usize) -> u8 {
        self.src.get(s).copied().unwrap_or(0)
    }

    fn do_match(&mut self, s: usize, p: usize) -> M {
        if self.depth == 0 {
            return Err("pattern too complex".into());
        }
        self.depth -= 1;
        let r = self.match_inner(s, p);
        self.depth += 1;
        r
    }

    fn match_inner(&mut self, mut s: usize, mut p: usize) -> M {
        loop {
            if p == self.pat.len() {
                return Ok(Some(s));
            }
            match self.pat[p] {
                b'(' => {
                    return if self.pat_at(p + 1) == b')' {
                        self.start_capture(s, p + 2, Len::Position)
                    } else {
                        self.start_capture(s, p + 1, Len::Unfinished)
                    };
                }
                b')' => return self.end_capture(s, p + 1),
                b'$' if p + 1 == self.pat.len() => {
                    return Ok((s == self.src.len()).then_some(s));
                }
                ESC if self.pat_at(p + 1) == b'b' => match self.match_balance(s, p + 2)? {
                    Some(e) => {
                        s = e;
                        p += 4;
                        continue;
                    }
                    None => return Ok(None),
                },
                ESC if self.pat_at(p + 1) == b'f' => {
                    p += 2;
                    if self.pat_at(p) != b'[' {
                        return Err("missing '[' after '%f' in pattern".into());
                    }
                    let ep = self.class_end(p)?;
                    let prev = if s == 0 { 0 } else { self.src[s - 1] };
                    let cur = self.src_at(s);
                    if !self.match_bracket(prev, p, ep - 1) && self.match_bracket(cur, p, ep - 1) {
                        p = ep;
                        continue;
                    }
                    return Ok(None);
                }
                ESC if self.pat_at(p + 1).is_ascii_digit() => {
                    match self.match_capture(s, self.pat_at(p + 1))? {
                        Some(e) => {
                            s = e;
                            p += 2;
                            continue;
                        }
                        None => return Ok(None),
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    let next = self.pat_at(ep);
                    if !self.single_match(s, p, ep) {
                        if matches!(next, b'*' | b'?' | b'-') && ep < self.pat.len() {
                            p = ep + 1;
                            continue;
                        }
                        return Ok(None);
                    }
                    match next {
                        b'?' if ep < self.pat.len() => {
                            if let Some(e) = self.do_match(s + 1, ep + 1)? {
                                return Ok(Some(e));
                            }
                            p = ep + 1;
                        }
                        b'+' if ep < self.pat.len() => return self.max_expand(s + 1, p, ep),
                        b'*' if ep < self.pat.len() => return self.max_expand(s, p, ep),
                        b'-' if ep < self.pat.len() => return self.min_expand(s, p, ep),
                        _ => {
                            s += 1;
                            p = ep;
                        }
                    }
                }
            }
        }
    }

    /// One past the end of the single-character class at `p`.
    fn class_end(&self, mut p: usize) -> Result<usize, String> {
        let c = self.pat[p];
        p += 1;
        if c == ESC {
            if p >= self.pat.len() {
                return Err("malformed pattern (ends with '%')".into());
            }
            return Ok(p + 1);
        }
        if c == b'[' {
            if self.pat_at(p) == b'^' {
                p += 1;
            }
            // look for the `]`, the first character might be a `]` itself
            loop {
                if p >= self.pat.len() {
                    return Err("malformed pattern (missing ']')".into());
                }
                let c = self.pat[p];
                p += 1;
                if c == ESC && p < self.pat.len() {
                    p += 1;
                }
                if self.pat_at(p) == b']' {
                    break;
                }
            }
            return Ok(p + 1);
        }
        Ok(p)
    }

    fn single_match(&self, s: usize, p: usize, ep: usize) -> bool {
        let Some(&c) = self.src.get(s) else {
            return false;
        };
        match self.pat[p] {
            b'.' => true,
            ESC => match_class(c, self.pat[p + 1]),
            b'[' => self.match_bracket(c, p, ep - 1),
            pc => pc == c,
        }
    }

    /// `c` against the set `[...]` from `p` (the `[`) to `ec` (the `]`).
    fn match_bracket(&self, c: u8, mut p: usize, ec: usize) -> bool {
        let mut sig = true;
        if self.pat_at(p + 1) == b'^' {
            sig = false;
            p += 1;
        }
        p += 1;
        while p < ec {
            if self.pat[p] == ESC {
                p += 1;
                if match_class(c, self.pat_at(p)) {
                    return sig;
                }
            } else if self.pat_at(p + 1) == b'-' && p + 2 < ec {
                if self.pat[p] <= c && c <= self.pat[p + 2] {
                    return sig;
                }
                p += 2;
            } else if self.pat[p] == c {
                return sig;
            }
            p += 1;
        }
        !sig
    }

    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> M {
        let mut i = 0;
        while self.single_match(s + i, p, ep) {
            i += 1;
        }
        loop {
            if let Some(e) = self.do_match(s + i, ep + 1)? {
                return Ok(Some(e));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(&mut self, mut s: usize, p: usize, ep: usize) -> M {
        loop {
            if let Some(e) = self.do_match(s, ep + 1)? {
                return Ok(Some(e));
            }
            if self.single_match(s, p, ep) {
                s += 1;
            } else {
                return Ok(None);
            }
        }
    }

    fn start_capture(&mut self, s: usize, p: usize, what: Len) -> M {
        if self.level >= MAX_CAPTURES {
            return Err("too many captures".into());
        }
        self.caps[self.level] = (s, what);
        self.level += 1;
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.level -= 1;
        }
        Ok(r)
    }

    fn end_capture(&mut self, s: usize, p: usize) -> M {
        let Some(l) = (0..self.level)
            .rev()
            .find(|&i| matches!(self.caps[i].1, Len::Unfinished))
        else {
            return Err("invalid pattern capture".into());
        };
        self.caps[l].1 = Len::Closed(s - self.caps[l].0);
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.caps[l].1 = Len::Unfinished;
        }
        Ok(r)
    }

    fn match_capture(&self, s: usize, digit: u8) -> M {
        let l = (digit as usize).wrapping_sub(b'1' as usize);
        let bad = || format!("invalid capture index %{}", digit as char);
        if l >= self.level {
            return Err(bad());
        }
        let (init, len) = self.caps[l];
        let n = match len {
            Len::Closed(n) => n,
            _ => return Err(bad()),
        };
        let cap = &self.src[init..init + n];
        Ok((self.src.len() - s >= n && &self.src[s..s + n] == cap).then_some(s + n))
    }

    fn match_balance(&self, s: usize, p: usize) -> M {
        if p + 1 >= self.pat.len() {
            return Err("malformed pattern (missing arguments to '%b')".into());
        }
        let (b, e) = (self.pat[p], self.pat[p + 1]);
        if self.src.get(s) != Some(&b) {
            return Ok(None);
        }
        let mut depth = 1;
        let mut i = s + 1;
        while i < self.src.len() {
            let c = self.src[i];
            if c == e {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(i + 1));
                }
            } else if c == b {
                depth += 1;
            }
            i += 1;
        }
        Ok(None)
    }
}

/// `%x` classes (C locale: ASCII only).
fn match_class(c: u8, class: u8) -> bool {
    let res = match class.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c.is_ascii_control(),
        b'd' => c.is_ascii_digit(),
        b'g' => c.is_ascii_graphic(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        _ => return class == c,
    };
    if class.is_ascii_uppercase() {
        !res
    } else {
        res
    }
}

/// `string.find`/`match` on `src` from byte `init`: the match's range and the
/// pattern's own captures (empty if it has none). `^...` patterns only try
/// `init`.
pub fn find(
    src: &[u8],
    pat: &[u8],
    init: usize,
) -> Result<Option<(usize, usize, Vec<Capture>)>, String> {
    let (anchor, p) = if pat.first() == Some(&b'^') {
        (true, 1)
    } else {
        (false, 0)
    };
    let mut m = Matcher::new(src, pat);
    let mut s = init;
    loop {
        if let Some(e) = m.try_at(s, p)? {
            return Ok(Some((s, e, m.explicit_captures()?)));
        }
        s += 1;
        if anchor || s > src.len() {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(src: &str, pat: &str) -> Option<Vec<String>> {
        let (s, e, mut caps) = find(src.as_bytes(), pat.as_bytes(), 0).unwrap()?;
        if caps.is_empty() {
            caps.push(Capture::Str(s, e));
        }
        Some(
            caps.into_iter()
                .map(|c| match c {
                    Capture::Str(a, b) => src[a..b].to_string(),
                    Capture::Pos(p) => format!("@{}", p + 1),
                })
                .collect(),
        )
    }

    #[test]
    fn classes_sets_and_quantifiers() {
        assert_eq!(caps("hello world", "%a+"), Some(vec!["hello".into()]));
        assert_eq!(
            caps("  x = 42;", "(%w+)%s*=%s*(%d+)"),
            Some(vec!["x".into(), "42".into()])
        );
        assert_eq!(caps("[[x]]", "%[(.-)%]"), Some(vec!["[x".into()]));
        assert_eq!(caps("aaab", "a-b"), Some(vec!["aaab".into()]));
        assert_eq!(
            caps("key=val", "^(%w+)=(%w*)$"),
            Some(vec!["key".into(), "val".into()])
        );
        assert_eq!(caps("x", "^y"), None);
        assert_eq!(caps("f(a(b)c)d", "%b()"), Some(vec!["(a(b)c)".into()]));
        assert_eq!(
            caps("THE (quick) fox", "%f[%a]%a+%f[%A]"),
            Some(vec!["THE".into()])
        );
        assert_eq!(
            caps("hello", "()ll()"),
            Some(vec!["@3".into(), "@5".into()])
        );
        assert_eq!(caps("abcabc", "(abc)%1"), Some(vec!["abc".into()]));
        assert_eq!(caps("a]b", "[]]"), Some(vec!["]".into()]));
        assert_eq!(caps("a-b", "[a%-]+"), Some(vec!["a-".into()]));
        assert_eq!(caps("0x1F", "[^%d]"), Some(vec!["x".into()]));
        assert_eq!(caps("ab", "a?b"), Some(vec!["ab".into()]));
        assert_eq!(caps("b", "a?b"), Some(vec!["b".into()]));
    }

    #[test]
    fn malformed_patterns() {
        for (pat, want) in [
            ("%", "malformed pattern (ends with '%')"),
            ("[a", "malformed pattern (missing ']')"),
            ("(a", "unfinished capture"),
            ("a)", "invalid pattern capture"),
            ("%1", "invalid capture index %1"),
            ("%f", "missing '[' after '%f' in pattern"),
            ("%b", "malformed pattern (missing arguments to '%b')"),
        ] {
            assert_eq!(find(b"a", pat.as_bytes(), 0).unwrap_err(), want, "{pat}");
        }
        let deep = "a?".repeat(300) + &"a".repeat(300);
        assert_eq!(
            find(&[b'a'; 300], deep.as_bytes(), 0).unwrap_err(),
            "pattern too complex"
        );
    }
}
