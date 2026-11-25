//! The after-merge gate's test shards (.github/workflows/main-gate.yml, `os::TEST_SHARDS`):
//! with `VELT_TEST_SHARD=<i>/<n>`, `--part test` builds as usual and runs every n-th test of each
//! test binary (nextest's `--partition count:<i>/<n>`). The steps that aren't nextest tests
//! (doctests, tests/difftest's unit tests, `velt fmt --check`, std-lint, the VS Code extension,
//! the smoke test) run in shard 1 only, so the shards together run exactly what one unsharded
//! `--part test` runs, each once.

use crate::golden_archive;

/// The test shard `(i, n)` from `VELT_TEST_SHARD` (empty or unset: none).
pub(super) fn from_env() -> Result<Option<(usize, usize)>, String> {
    parse(std::env::var("VELT_TEST_SHARD").ok().as_deref())
}

fn parse(value: Option<&str>) -> Result<Option<(usize, usize)>, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => golden_archive::parse_shard_of("VELT_TEST_SHARD", v).map(Some),
    }
}

/// nextest's arguments for `shard`: none without one.
pub(super) fn partition_args(shard: Option<(usize, usize)>) -> Vec<String> {
    match shard {
        Some((i, n)) => vec!["--partition".into(), format!("count:{i}/{n}")],
        None => vec![],
    }
}

/// Whether this run takes the steps that aren't nextest tests: unsharded, or shard 1.
pub(super) fn runs_the_other_steps(shard: Option<(usize, usize)>) -> bool {
    shard.is_none_or(|(i, _)| i == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_shard_is_parsed_and_checked() {
        assert_eq!(parse(None), Ok(None));
        assert_eq!(parse(Some("")), Ok(None));
        assert_eq!(parse(Some("2/3")), Ok(Some((2, 3))));
        for bad in ["0/3", "4/3", "3", "a/b", "1/0"] {
            let err = parse(Some(bad)).unwrap_err();
            assert!(err.starts_with("VELT_TEST_SHARD="), "{err}");
        }
    }

    #[test]
    fn every_test_runs_in_exactly_one_shard_and_the_other_steps_in_the_first() {
        assert!(partition_args(None).is_empty());
        assert_eq!(partition_args(Some((2, 3))), ["--partition", "count:2/3"]);
        assert!(runs_the_other_steps(None));
        let n = 3;
        let firsts: Vec<bool> = (1..=n)
            .map(|i| runs_the_other_steps(Some((i, n))))
            .collect();
        assert_eq!(firsts, [true, false, false]);
    }
}
