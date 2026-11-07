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

use std::path::Path;
use std::process::Command;

use crate::check::{exe, target_dir};
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

/// The steps that build the C bundles the plan's goldens link, before the goldens start: in each
/// of [`PACKAGES`], `velt native build` writes `target/velt-native/<host>`, where a dependent's
/// build looks for it and finds it current. Its key covers neither the package's path nor the
/// profile, so the debug and release builds of a golden use it, and so does a build that reached
/// the package by its canonical path. A step has no time limit, while the golden harness gives
/// each build 120 s to catch hangs, and a cold build of SQLite on a Windows shard takes longer
/// (#2347). With the bundle cache warm, a step only hashes the sources.
pub fn bundle_builds(root: &Path, plan: &Plan) -> Vec<(String, Command)> {
    if !goldens(plan) {
        return vec![];
    }
    let velt = target_dir(root).join("debug").join(exe("velt"));
    (PACKAGES.iter())
        .map(|p| {
            let mut build = Command::new(&velt);
            build
                .args(["native", "build"])
                .current_dir(root.join("packages").join(p));
            (
                format!("C bundle of packages/{p}, before the goldens"),
                build,
            )
        })
        .collect()
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

/// The code besides [`PACKAGES`] and [`DEPENDENTS`] that a build of a dependent goes through
/// differently on Windows: the check of a package's `declare c` against its headers, the
/// constants read from them, and the build of its C `sources`, which hand zig the package's
/// path (a verbatim `\\?\` path reached clang there and only the merge queue saw it, #2347).
pub const WINDOWS_APP_CODE: &[&str] = &[
    "crates/veltc/src/native/headers/",
    "crates/vpm/src/native/c_build.rs",
    "crates/vpm/src/install/c_sources.rs",
];

/// The `VELT_GOLDEN` filter of the [`DEPENDENTS`]' goldens that a pull request's Windows job runs,
/// with their `velt test` (`example_apps`): all of them, separated by `,`, when the change touches
/// one of [`PACKAGES`], a dependent, or [`WINDOWS_APP_CODE`]; `""` otherwise. There a package is
/// a dependency of the program, at a path the program's build canonicalized, which its own
/// tests (`packages`) never are. `skip` are files where only comments changed; documentation
/// alone changes nothing either.
pub fn windows_apps(paths: &[String], skip: &std::collections::BTreeSet<String>) -> String {
    let touched = paths
        .iter()
        .filter(|p| !skip.contains(*p) && !p.ends_with(".md"))
        .any(|p| builds(p) || WINDOWS_APP_CODE.iter().any(|c| p.starts_with(c)));
    match touched {
        true => DEPENDENTS.join(","),
        false => String::new(),
    }
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

    /// The pull request's Windows job builds each package's C bundle before its tests, as the
    /// gate does before the goldens ([`bundle_builds`]).
    #[test]
    fn the_windows_pull_request_job_builds_each_c_bundle_first() {
        let ci = std::fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("ci.yml");
        for p in PACKAGES {
            let step = format!("(cd packages/{p} && ../../target/debug/velt native build)");
            assert!(ci.contains(&step), "ci.yml has no `{step}`");
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
    fn a_change_to_a_c_package_or_its_build_runs_the_dependents_on_windows() {
        let none = std::collections::BTreeSet::new();
        let apps = |paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            windows_apps(&paths, &none)
        };
        let all = DEPENDENTS.join(",");
        assert_eq!(apps(&["packages/sqlite/src/sqlite3.velt"]), all);
        assert_eq!(apps(&["packages/sqlite/native/sqlite3.h"]), all);
        assert_eq!(apps(&["examples/apps/todo-api/src/store.velt"]), all);
        assert_eq!(apps(&["crates/veltc/src/native/headers/mod.rs"]), all);
        assert_eq!(apps(&["crates/vpm/src/native/c_build.rs"]), all);
        assert_eq!(apps(&["packages/sqlite/README.md"]), "");
        assert_eq!(apps(&["packages/redis/src/client.velt"]), "");
        assert_eq!(apps(&["crates/veltc/src/native/report.rs"]), "");
        assert_eq!(apps(&[]), "");
        let skip = std::collections::BTreeSet::from(["packages/sqlite/src/a.velt".to_string()]);
        assert_eq!(
            windows_apps(&["packages/sqlite/src/a.velt".to_string()], &skip),
            ""
        );
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
