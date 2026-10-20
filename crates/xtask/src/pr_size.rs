//! The size of a pull request, as a non-blocking note from `cargo xtask pr-lint`.
//!
//! Large pull requests take several times longer to merge and get more change rounds than small
//! ones, so CONTRIBUTING.md asks for a stack once a change grows past about [`SOFT_CAP`] changed
//! lines. The note only informs: it never fails the check, and a missing or failing `git diff`
//! (a shallow or partial clone) just means no note.

use std::path::Path;

/// Added plus deleted lines past which a pull request should usually be split into a stack.
pub const SOFT_CAP: usize = 1500;

/// Added plus deleted lines in `git diff --numstat` output; binary files (`-`) count as none.
pub fn changed_lines(numstat: &str) -> usize {
    numstat
        .lines()
        .flat_map(|l| l.split('\t').take(2))
        .filter_map(|n| n.parse::<usize>().ok())
        .sum()
}

/// The note for a pull request of `lines` changed lines, when it is over the cap.
pub fn note(lines: usize) -> Option<String> {
    (lines > SOFT_CAP).then(|| {
        format!(
            "{lines} changed lines, over the soft cap of about {SOFT_CAP}: consider splitting \
             it into a stack of pull requests (CONTRIBUTING.md, \"Commits and pull requests\")"
        )
    })
}

/// The note for the changes from the merge base of `base` and `head` to `head`, if any.
pub fn check(root: &Path, base: &str, head: &str) -> Option<String> {
    let range = format!("{base}...{head}");
    let numstat = crate::pr_lint::git(root, &["diff", "--numstat", &range]).ok()?;
    note(changed_lines(&numstat))
}

/// A GitHub Actions `::warning`: shown in the checks summary without failing the job.
pub fn annotation(note: &str) -> String {
    format!(
        "::warning title=pr-lint%3A size::{}\n",
        note.replace('%', "%25")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_added_and_deleted_lines_and_skips_binary_files() {
        let numstat = "10\t2\tsrc/a.rs\n-\t-\tlogo.png\n0\t7\tdocs/b.md\n";
        assert_eq!(changed_lines(numstat), 19);
        assert_eq!(changed_lines(""), 0);
    }

    #[test]
    fn only_a_pull_request_over_the_cap_gets_a_note() {
        assert_eq!(note(SOFT_CAP), None);
        let n = note(SOFT_CAP + 1).expect("over the cap");
        assert!(n.starts_with("1501 changed lines"), "{n}");
        assert!(n.contains("stack"), "{n}");
    }

    #[test]
    fn the_annotation_is_a_warning_not_an_error() {
        let a = annotation("2000 changed lines, 100% too many");
        assert!(
            a.starts_with("::warning title=pr-lint%3A size::2000"),
            "{a}"
        );
        assert!(a.contains("100%25 too many"), "{a}");
    }
}
