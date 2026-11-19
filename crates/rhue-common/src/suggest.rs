//! "Did you mean ...?" Finds the closest name to a misspelled one.

/// The candidate closest to `name` by edit distance (swapping two neighbours
/// counts as one edit), if it's close enough to be a typo: one edit for names
/// up to 4 chars, two up to 8, three past that. Ties go to whichever came
/// first, and `name` never matches itself.
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = match name.chars().count() {
        0..=1 => return None,
        2..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let d = distance(name, c);
        // case alone (`Print` for `print`) is always worth suggesting
        let d = if name.eq_ignore_ascii_case(c) { 1 } else { d };
        if d <= limit && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Optimal string alignment distance over chars.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 3 {
        return usize::MAX;
    }
    let w = b.len() + 1;
    let mut d = vec![0usize; (a.len() + 1) * w];
    for i in 0..=a.len() {
        d[i * w] = i;
    }
    for (j, cell) in d.iter_mut().take(w).enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[(i - 1) * w + j] + 1)
                .min(d[i * w + j - 1] + 1)
                .min(d[(i - 1) * w + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[(i - 2) * w + j - 2] + 1);
            }
            d[i * w + j] = v;
        }
    }
    d[a.len() * w + b.len()]
}

#[cfg(test)]
mod tests {
    use super::closest;

    #[test]
    fn typos() {
        let names = ["print", "pairs", "ipairs", "tostring", "position"];
        assert_eq!(closest("pirnt", names), Some("print"));
        assert_eq!(closest("prnt", names), Some("print"));
        assert_eq!(closest("Print", names), Some("print"));
        assert_eq!(closest("tostrng", names), Some("tostring"));
        assert_eq!(closest("postion", names), Some("position"));
        assert_eq!(closest("xyz", names), None);
        assert_eq!(closest("print", names), None);
        assert_eq!(closest("p", names), None);
        // short names need to be close
        assert_eq!(closest("nme", ["name", "age"]), Some("name"));
        assert_eq!(closest("ag", ["name", "x"]), None);
    }
}
