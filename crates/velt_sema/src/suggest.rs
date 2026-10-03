//! "Did you mean …?" suggestions.

/// The longest edit distance a "Did you mean" suggestion may have.
const MAX_SUGGESTION_DISTANCE: usize = 2;

/// The name in `known` closest to `name` (case-insensitive equal, or within a small edit
/// distance).
pub(crate) fn closest<'k>(name: &str, known: &'k [String]) -> Option<&'k str> {
    known
        .iter()
        .map(|k| (distance(&name.to_lowercase(), &k.to_lowercase()), k))
        .filter(|(d, _)| *d <= MAX_SUGGESTION_DISTANCE)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k.as_str())
}

/// Levenshtein distance over chars.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        let known = vec!["class".to_string(), "href".to_string(), "id".to_string()];
        assert_eq!(closest("clas", &known), Some("class"));
        assert_eq!(closest("Class", &known), Some("class"));
        assert_eq!(closest("hreff", &known), Some("href"));
        assert_eq!(closest("onClick", &known), None);
    }

    #[test]
    fn edit_distance() {
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("same", "same"), 0);
    }
}
