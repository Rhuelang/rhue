use std::fmt;
use std::ops::Range;

/// A half-open byte range `start..end` into a source string.
///
/// Spans are `u32` to keep tokens, AST nodes, and per-instruction debug info
/// small; sources larger than 4 GiB are rejected up front by the lexer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    /// Slices `source` with this span.
    pub fn text(self, source: &str) -> &str {
        &source[self.range()]
    }
}

impl From<Range<usize>> for Span {
    fn from(r: Range<usize>) -> Self {
        Span::new(r.start as u32, r.end as u32)
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}
