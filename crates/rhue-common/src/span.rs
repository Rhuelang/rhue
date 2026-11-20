use std::fmt;
use std::ops::Range;

/// An offset or length in a source as a `u32`. The lexer refuses anything over
/// 4 GiB, so anything measured in a source that got that far fits. Counts of
/// things in a source (tokens, locals, instructions) fit for the same reason.
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "see above, sources are under 4 GiB"
)]
pub fn offset(n: usize) -> u32 {
    debug_assert!(u32::try_from(n).is_ok(), "{n} doesn't fit a source offset");
    n as u32
}

/// A half-open byte range `start..end` into some source.
///
/// Spans are `u32` to keep tokens, AST nodes and per-instruction debug info
/// small. The lexer refuses anything over 4 GiB up front.
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
        Span::new(offset(r.start), offset(r.end))
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}
