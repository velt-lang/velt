//! What uses the repository's packages (`packages/<name>/`) outside `packages/`: the programs and
//! documents that depend on a package by path, whose tests a change to the package selects
//! besides the packages' own (crates/xtask/src/plan.rs, `READ_BY_TESTS`).
//!
//! The table is kept by hand and checked against the repository: [`tests`] finds every
//! `package.vlt` outside `packages/` with a path dependency on a package, every Rust test that
//! names a package's directory (`packages/<name>`, as a manifest it writes at run time does), and
//! every package the documentation test compiles snippets against, and fails when the plan for
//! a change to that package would not run their tests.

/// A package (`packages/<name>/`): the goldens that depend on it (`VELT_GOLDEN` substrings) and
/// the `veltc` test binaries that compile programs or documents using it.
pub(crate) struct Users {
    pub(crate) package: &'static str,
    pub(crate) goldens: &'static [&'static str],
    pub(crate) binaries: &'static [&'static str],
}

pub(crate) const USERS: &[Users] = &[
    Users {
        package: "http-server",
        goldens: &[
            "tests/golden/std/http_server/",
            "tests/golden/lang/errors/http_server/",
        ],
        // tests/std (the `fetch` and WebSocket clients' tests talk to it), the server benchmarks
        // (`bench_programs` compiles them), the documentation (`docs` compiles its snippets
        // against the packages without native code, and reads packages/http-server's README and
        // docs), and tests that write manifests depending on it: hot reload's servers (`reload`)
        // and the `api` and `websocket` templates' projects (`templates`).
        binaries: &["std_tests", "bench_programs", "docs", "reload", "templates"],
    },
    // The database benchmarks (bench/db, bench/sqlite, bench/web, ...).
    Users {
        package: "sqlite",
        goldens: &[],
        binaries: &["bench_programs"],
    },
    Users {
        package: "postgres",
        goldens: &[],
        binaries: &["bench_programs"],
    },
    Users {
        package: "redis",
        goldens: &[],
        binaries: &["bench_programs"],
    },
];

/// The users of the package `path` is in (`packages/<name>/…`), if the table lists it.
pub(crate) fn of_path(path: &str) -> Option<&'static Users> {
    let name = path.strip_prefix("packages/")?.split('/').next()?;
    USERS.iter().find(|u| u.package == name)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::graph::Graph;
    use crate::plan::{Goldens, Plan, Veltc};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root")
    }

    /// Every `package.vlt` under `dir` (relative to `root`, `/`-separated), skipping build
    /// output and installed dependencies.
    fn manifests(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !(name.starts_with('.') || name == "target" || name == "node_modules") {
                    manifests(root, &path, out);
                }
            } else if name == "package.vlt" {
                let rel = path.strip_prefix(root).expect("under the root");
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// The packages of `packages/` that `manifest` (its text) depends on by path.
    fn path_dependencies(manifest: &str) -> Vec<String> {
        let mut out = vec![];
        for (i, _) in manifest.match_indices("packages/") {
            let rest = &manifest[i + "packages/".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let quoted = manifest[..i].rfind('"').is_some_and(|q| {
                let between = &manifest[q + 1..i];
                between.chars().all(|c| c == '.' || c == '/')
            });
            if quoted && !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    fn plan_for(package: &str) -> Plan {
        Plan::for_paths(
            &Graph::default(),
            &[format!("packages/{package}/src/lib.vlt")],
        )
    }

    fn runs(plan: &Plan, binary: &str) -> bool {
        match &plan.veltc {
            Veltc::All => true,
            Veltc::Some(set) => set.contains(binary),
            Veltc::None => false,
        }
    }

    fn runs_goldens_in(plan: &Plan, dir: &str) -> bool {
        match &plan.goldens {
            Goldens::All => true,
            Goldens::Matching(filters) => filters.iter().any(|f| dir.starts_with(f.as_str())),
            Goldens::None => false,
        }
    }

    #[test]
    fn manifests_name_their_path_dependencies() {
        let text = r#"dependencies: { "http-server": { path: "../../packages/http-server" },
            sqlite: { path: "../packages/sqlite" } }, // see packages/redis"#;
        assert_eq!(path_dependencies(text), ["http-server", "sqlite"]);
    }

    /// Each program depending on a package by path is tested when the package changes.
    #[test]
    fn a_changed_package_runs_the_tests_of_what_depends_on_it() {
        let root = root();
        let mut found = vec![];
        for top in ["tests", "bench", "examples", "docs"] {
            manifests(&root, &root.join(top), &mut found);
        }
        assert!(!found.is_empty(), "no package.vlt outside packages/");
        let mut missing = vec![];
        for manifest in &found {
            let text = std::fs::read_to_string(root.join(manifest)).expect("manifest");
            let dir = manifest.strip_suffix("package.vlt").expect("package.vlt");
            for package in path_dependencies(&text) {
                let plan = plan_for(&package);
                let tested = if dir.starts_with("tests/golden/") {
                    runs_goldens_in(&plan, dir)
                } else if dir.starts_with("tests/std/") {
                    runs(&plan, "std_tests")
                } else if dir.starts_with("bench/") {
                    runs(&plan, "bench_programs")
                } else if dir.starts_with("examples/") {
                    runs(&plan, "example_apps")
                } else {
                    false
                };
                if !tested {
                    missing.push(format!("{manifest} depends on packages/{package}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "a change to the package would not test these (add them to package_users::USERS, \
             or teach the test where they live):\n{}",
            missing.join("\n")
        );
    }

    /// Each Rust test (`crates/<crate>/tests/<name>.rs`) that names a package's directory runs
    /// when the package changes: `veltc`'s test binary `<name>`, another crate's tests.
    #[test]
    fn a_changed_package_runs_the_rust_tests_naming_it() {
        let root = root();
        let packages: Vec<String> = std::fs::read_dir(root.join("packages"))
            .expect("packages/")
            .flatten()
            .filter(|e| e.path().join("package.vlt").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let mut missing = vec![];
        let crates = std::fs::read_dir(root.join("crates")).expect("crates/");
        for krate in crates.flatten() {
            let Ok(tests) = std::fs::read_dir(krate.path().join("tests")) else {
                continue;
            };
            let krate = krate.file_name().to_string_lossy().into_owned();
            for test in tests.flatten() {
                let path = test.path();
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for package in &packages {
                    let named = text
                        .match_indices(&format!("packages/{package}"))
                        .any(|(i, m)| {
                            let next = text[i + m.len()..].chars().next();
                            !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        });
                    if !named {
                        continue;
                    }
                    let plan = plan_for(package);
                    let tested = if krate == "veltc" {
                        runs(&plan, &name)
                    } else {
                        plan.all_tests || plan.packages.contains(&krate)
                    };
                    if !tested {
                        missing.push(format!(
                            "crates/{krate}/tests/{name}.rs names packages/{package}"
                        ));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "a change to the package would not run these (add them to package_users::USERS):\n{}",
            missing.join("\n")
        );
    }

    /// The documentation test compiles snippets against every package without native code.
    #[test]
    fn a_changed_package_without_native_code_runs_the_documentation_test() {
        let root = root();
        let entries = std::fs::read_dir(root.join("packages")).expect("packages/");
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.join("package.vlt").is_file() || dir.join("native").is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(runs(&plan_for(&name), "docs"), "packages/{name}");
        }
    }

    #[test]
    fn every_listed_golden_directory_exists() {
        let root = root();
        for users in super::USERS {
            assert!(
                root.join("packages").join(users.package).is_dir(),
                "{}",
                users.package
            );
            for dir in users.goldens {
                assert!(root.join(dir).is_dir(), "{dir}");
            }
        }
    }
}
