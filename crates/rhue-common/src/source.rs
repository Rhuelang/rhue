use std::rc::Rc;

/// A named chunk of source text. Every prototype compiled from it holds an `Rc`
/// to it, so a runtime error always renders against the right file, even when a
/// function from one `eval` gets called from another.
#[derive(Debug)]
pub struct SourceFile {
    pub name: Rc<str>,
    pub text: Rc<str>,
    /// Byte offsets where each line starts, built the first time
    /// [`line`](Self::line) needs it.
    line_starts: std::cell::OnceCell<Vec<u32>>,
}

impl SourceFile {
    pub fn new(name: impl Into<Rc<str>>, text: impl Into<Rc<str>>) -> Rc<Self> {
        Rc::new(SourceFile {
            name: name.into(),
            text: text.into(),
            line_starts: std::cell::OnceCell::new(),
        })
    }

    /// 1-based line of a byte offset. Binary searches a line table that gets
    /// built on first use (debug hooks ask this every instruction).
    pub fn line(&self, offset: u32) -> u32 {
        let starts = self.line_starts.get_or_init(|| {
            let mut v = vec![0u32];
            v.extend(
                self.text
                    .match_indices('\n')
                    .map(|(i, _)| crate::offset(i) + 1),
            );
            v
        });
        crate::offset(starts.partition_point(|&s| s <= offset))
    }

    /// 1-based line and column (in characters) of a byte offset.
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let offset = (offset as usize).min(self.text.len());
        let before = &self.text[..offset];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let col = before[line_start..].chars().count() + 1;
        (line, col)
    }
}

/// Equal when the name and text are, the line table's just a cache.
impl PartialEq for SourceFile {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.text == other.text
    }
}

impl Eq for SourceFile {}

impl Default for SourceFile {
    fn default() -> Self {
        SourceFile {
            name: "<unknown>".into(),
            text: "".into(),
            line_starts: std::cell::OnceCell::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col() {
        let s = SourceFile::new("t", "ab\ncdé\nf");
        assert_eq!(s.line_col(0), (1, 1));
        assert_eq!(s.line_col(3), (2, 1));
        assert_eq!(s.line_col(7), (2, 4));
        assert_eq!(s.line_col(8), (3, 1));
        assert_eq!(s.line_col(9), (3, 2));
    }
}
