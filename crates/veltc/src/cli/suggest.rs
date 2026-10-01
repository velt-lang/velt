//! "Did you mean …?" for mistyped commands, options and template names: the closest candidate by
//! edit distance, if it is close enough to be a plausible typo.

/// The candidate closest to `input`, if within a typo's distance (a third of the length, at
/// least 1; exact prefix matches such as `--rel` for `--release` also count).
pub fn closest<'a>(input: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let limit = (input.chars().count() / 3).max(1);
    let by_distance = candidates
        .iter()
        .map(|c| (distance(input, c), *c))
        .filter(|(d, _)| *d <= limit)
        // Ties go to a candidate with the same first letter (`fsh`: `fish`, not `zsh`).
        .min_by_key(|(d, c)| (*d, input.chars().next() != c.chars().next()))
        .map(|(_, c)| c);
    by_distance.or_else(|| {
        let mut prefixed = candidates
            .iter()
            .filter(|c| input.len() >= 3 && c.starts_with(input));
        match (prefixed.next(), prefixed.next()) {
            (Some(only), None) => Some(*only),
            _ => None,
        }
    })
}

/// ` (did you mean `x`?)`-style suffix for an error message, or "".
pub fn hint(input: &str, candidates: &[&str]) -> String {
    closest(input, candidates)
        .map(|c| format!("; did you mean `{c}`?"))
        .unwrap_or_default()
}

/// Optimal string alignment distance (Levenshtein plus adjacent transpositions, so `biuld` is
/// one edit from `build`).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[&str] = &["build", "run", "dev", "test", "fmt", "new", "init", "clean"];

    #[test]
    fn typos_find_the_command() {
        assert_eq!(closest("biuld", COMMANDS), Some("build"));
        assert_eq!(closest("buidl", COMMANDS), Some("build"));
        assert_eq!(closest("rn", COMMANDS), Some("run"));
        assert_eq!(closest("tset", COMMANDS), Some("test"));
        assert_eq!(closest("cleen", COMMANDS), Some("clean"));
        assert_eq!(closest("fsh", &["bash", "zsh", "fish"]), Some("fish"));
    }

    #[test]
    fn unrelated_words_find_nothing() {
        assert_eq!(closest("frobnicate", COMMANDS), None);
        assert_eq!(closest("xyz", COMMANDS), None);
        assert_eq!(hint("xyz", COMMANDS), "");
    }

    #[test]
    fn options_and_prefixes() {
        let flags = ["--release", "--locked", "--target", "--backend"];
        assert_eq!(closest("--relase", &flags), Some("--release"));
        assert_eq!(closest("--lock", &flags), Some("--locked"));
        assert_eq!(closest("--rel", &flags), Some("--release"));
        assert_eq!(hint("--taget", &flags), "; did you mean `--target`?");
    }

    #[test]
    fn distance_counts_edits() {
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("abc", "abc"), 0);
        assert_eq!(distance("ab", "ba"), 1);
        assert_eq!(distance("kitten", "sitting"), 3);
    }
}
