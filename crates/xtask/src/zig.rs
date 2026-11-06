//! Which test runs build a package's C `sources`, which takes the pinned zig (#2385): only the CI
//! jobs that run them get it from the Actions cache (.github/actions/zig, which sets `VELT_ZIG`),
//! so no other pull request or queue job restores it, and none downloads it from ziglang.org on
//! first use either. A rule that misses a test costs a download, not a failure: without
//! `VELT_ZIG`, velt downloads the pinned zig itself.
//!
//! A `--full` plan selects every such test, so the merge queue's checks and goldens, the checks
//! after merging on Windows and macOS, the nightly gate and the release gate get zig whenever
//! they run. The tests that check through zig for every target are another matter: the
//! `c-headers` job runs them (crates/xtask/src/c_headers.rs, `VELT_ZIG_TESTS`).

use crate::plan::{Goldens, Plan, Veltc};

/// The repository's packages with C `sources` (`packages/<name>/`).
pub const PACKAGES: &[&str] = &["sqlite"];

/// The programs outside `packages/` that depend on one of [`PACKAGES`] by path and are tests
/// themselves, by directory: an example app's `velt test` (`example_apps`) and its goldens
/// (`demo*.velt` with an `.out`). The benchmarks that do (bench/chat, bench/db, bench/feed,
/// bench/sqlite) are compiled by `bench_programs` and are no test file of their own.
pub const DEPENDENTS: &[&str] = &["examples/apps/todo-api/"];

/// The `veltc` test binaries that build them: each package's `velt test` (`packages`), the
/// example apps' (`example_apps`) and the benchmarks (`bench_programs`).
pub const BINARIES: &[&str] = &["packages", "example_apps", "bench_programs"];

/// Whether the plan's tests (the `test` part) build C `sources`.
pub fn tests(plan: &Plan) -> bool {
    match &plan.veltc {
        Veltc::All => true,
        Veltc::Some(binaries) => BINARIES.iter().any(|b| binaries.contains(*b)),
        Veltc::None => false,
    }
}

/// Whether the plan's goldens (the `golden` part) build C `sources`: a `VELT_GOLDEN` filter is a
/// substring of the path, and the plan's filters are path prefixes.
pub fn goldens(plan: &Plan) -> bool {
    match &plan.goldens {
        Goldens::All => true,
        Goldens::Matching(filters) => filters.iter().any(|f| {
            DEPENDENTS
                .iter()
                .any(|d| d.starts_with(f.as_str()) || f.starts_with(d))
        }),
        Goldens::None => false,
    }
}

/// Whether the test file or golden `path` builds C `sources`: a test of one of [`PACKAGES`] or
/// of one of the [`DEPENDENTS`], for what a pull request repeats under load (`cargo xtask
/// repeat`).
pub fn builds(path: &str) -> bool {
    let package = path
        .strip_prefix("packages/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(name, _)| PACKAGES.contains(&name));
    package || DEPENDENTS.iter().any(|d| path.starts_with(d))
}

/// Whether a pull request's Windows job builds C `sources`: the packages whose tests it runs
/// (`VELT_PACKAGES`: `""`, `"all"` or names, crates/xtask/src/os.rs) or the files it repeats.
pub fn windows(packages: &str, repeat: &[String]) -> bool {
    let tested = packages == "all" || packages.split(',').any(|p| PACKAGES.contains(&p));
    tested || repeat.iter().any(|p| builds(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root")
    }

    /// Every file under `dir` (relative to `root`, `/`-separated), skipping build output and
    /// installed dependencies.
    fn files(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !(name.starts_with('.') || name == "target" || name == "node_modules") {
                    files(root, &path, out);
                }
            } else {
                let rel = path.strip_prefix(root).expect("under the root");
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn packages_lists_every_package_with_c_sources() {
        let root = root();
        let mut found: Vec<String> = std::fs::read_dir(root.join("packages"))
            .expect("packages/")
            .flatten()
            .filter(|e| {
                let manifest = std::fs::read_to_string(e.path().join("package.velt"));
                manifest.is_ok_and(|m| m.contains("sources:"))
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, PACKAGES, "update zig::PACKAGES");
    }

    /// The zig action restores each package's C bundle, built from what its key hashes.
    #[test]
    fn the_zig_action_caches_each_package_s_c_bundle() {
        let action = std::fs::read_to_string(root().join(".github/actions/zig/action.yml"))
            .expect("the zig action");
        for p in PACKAGES {
            for named in [
                format!("packages/{p}/target/velt-native"),
                format!("'packages/{p}/package.velt'"),
                format!("'packages/{p}/native/**'"),
            ] {
                assert!(action.contains(&named), "the zig action names no {named}");
            }
        }
    }

    /// Every use of the zig action passes the same absolute working directory: actions/cache
    /// keys on the literal `path` string, so `./packages/...` and the absolute path are two
    /// caches, and a merge queue entry would never restore the bundle `main` saved.
    #[test]
    fn every_use_of_the_zig_action_names_one_working_directory() {
        let root = root();
        let wanted = "working-directory: ${{ env.DEV_DRIVE_WORKSPACE || github.workspace }}";
        let mut missing = vec![];
        let mut uses = 0;
        for entry in std::fs::read_dir(root.join(".github/workflows"))
            .expect("workflows")
            .flatten()
        {
            let path = entry.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim() != "- uses: ./.github/actions/zig" {
                    continue;
                }
                uses += 1;
                let indent = line.len() - line.trim_start().len();
                // The step runs until the next line at the step's indentation or less.
                let named = lines[i + 1..]
                    .iter()
                    .take_while(|l| l.trim().is_empty() || l.len() - l.trim_start().len() > indent)
                    .any(|l| l.trim() == wanted);
                if !named {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    missing.push(format!("{name}:{}", i + 1));
                }
            }
        }
        assert!(uses > 0, "no workflow uses the zig action");
        // GitHub evaluates `${{` in an action's descriptions too (and fails every job that uses
        // it on `env`), which actionlint doesn't check in composite actions.
        let action = std::fs::read_to_string(root.join(".github/actions/zig/action.yml"))
            .expect("the zig action");
        let head = action.split("\nruns:").next().unwrap_or_default();
        assert!(
            !head.contains("${{"),
            "the zig action's descriptions hold an expression"
        );
        assert!(
            missing.is_empty(),
            "these pass no `{wanted}`: {}",
            missing.join(", ")
        );
    }

    /// Each program outside `packages/` that depends on a package with C `sources` is a
    /// benchmark (`bench_programs`) or one of the [`DEPENDENTS`].
    #[test]
    fn dependents_lists_every_test_that_depends_on_them() {
        let root = root();
        let mut all = vec![];
        for top in ["tests", "bench", "examples", "docs"] {
            files(&root, &root.join(top), &mut all);
        }
        let mut missing = vec![];
        for manifest in all.iter().filter(|f| f.ends_with("package.velt")) {
            let text = std::fs::read_to_string(root.join(manifest)).expect("manifest");
            let dir = manifest.strip_suffix("package.velt").expect("package.velt");
            let depends = PACKAGES
                .iter()
                .any(|p| text.contains(&format!("packages/{p}\"")));
            let covered = dir.starts_with("bench/") || DEPENDENTS.contains(&dir);
            if depends && !covered {
                missing.push(manifest.clone());
            }
        }
        assert!(
            missing.is_empty(),
            "these depend on a package with C sources: add them to zig::DEPENDENTS:\n{}",
            missing.join("\n")
        );
        for dir in DEPENDENTS {
            assert!(root.join(dir).join("package.velt").is_file(), "{dir}");
        }
    }

    #[test]
    fn repeated_files_and_windows_packages_select_zig_when_they_build_c_sources() {
        assert!(builds("packages/sqlite/tests/driver.test.velt"));
        assert!(builds("examples/apps/todo-api/tests/api.test.velt"));
        assert!(builds("examples/apps/todo-api/demo_sqlite.velt"));
        assert!(!builds("packages/sqlite-extra/tests/a.test.velt"));
        assert!(!builds("packages/redis/tests/client.test.velt"));
        assert!(!builds("examples/apps/todo-apis/demo.velt"));
        assert!(!builds("tests/golden/std/fs/read.velt"));

        let none: Vec<String> = vec![];
        assert!(windows("all", &none));
        assert!(windows("redis,sqlite", &none));
        assert!(!windows("redis,http-server", &none));
        assert!(!windows("", &none));
        assert!(windows(
            "",
            &["packages/sqlite/tests/tx.test.velt".to_string()]
        ));
        assert!(!windows(
            "",
            &["tests/golden/std/timers/a.velt".to_string()]
        ));
    }
}
