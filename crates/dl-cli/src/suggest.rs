//! "Did you mean" suggestions based on the optimal string alignment distance
//! (Levenshtein plus transposition of adjacent characters).

/// Largest edit distance that still produces a suggestion.
const MAX_DISTANCE: usize = 2;

/// Optimal string alignment distance between `a` and `b`, counted in `char`s.
pub(crate) fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(before[j - 2] + 1);
            }
            cur[j] = d;
        }
        core::mem::swap(&mut before, &mut prev);
        core::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The candidate closest to `input` (case-insensitive), if it is within
/// [`MAX_DISTANCE`] edits and shares at least one character position with it.
/// Ties go to the first candidate.
pub(crate) fn suggest<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let input_lower = input.to_lowercase();
    let input_len = input_lower.chars().count();
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in candidates {
        let lower = candidate.to_lowercase();
        // Cheap length filter first: argv tokens are untrusted and may be long.
        if lower.chars().count().abs_diff(input_len) > MAX_DISTANCE {
            continue;
        }
        let d = distance(&input_lower, &lower);
        if d <= MAX_DISTANCE && d < input_len && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, candidate));
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        assert_eq!(distance("", ""), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("abc", ""), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("flaw", "lawn"), 2);
        assert_eq!(distance("prot", "port"), 1); // transposition
        assert_eq!(distance("ca", "abc"), 3); // OSA, not unrestricted Damerau (2)
        assert_eq!(distance("serve", "serve"), 0);
        assert_eq!(distance("héllo", "hello"), 1); // counted in chars
    }

    #[test]
    fn suggestions() {
        let names = ["port", "format", "verbose", "include"];
        assert_eq!(suggest("prot", names), Some("port"));
        assert_eq!(suggest("formt", names), Some("format"));
        assert_eq!(suggest("verbsoe", names), Some("verbose"));
        assert_eq!(suggest("PORT", names), Some("port"));
        assert_eq!(suggest("xyz", names), None);
        assert_eq!(suggest("p", names), None);
        assert_eq!(suggest("x", ["ls"]), None);
        assert_eq!(suggest("lx", ["ls"]), Some("ls"));
        assert_eq!(suggest("abcd", ["abce", "abcf"]), Some("abce"));
        assert_eq!(suggest("a".repeat(10_000).as_str(), names), None);
    }
}
