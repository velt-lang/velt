//! Which crates have documentation tests: a fenced code block in a `///` or `//!` comment that
//! rustdoc compiles (no info string, or only `rust`, `compile_fail`, `no_run`, `should_panic`
//! and `edition…`). `cargo test --doc --workspace` runs rustdoc over every crate and resolves
//! features unlike the build, so it compiled part of the workspace again; testing only these
//! crates takes seconds.

use std::path::Path;

/// The packages (by directory under `crates/`, mapped through `dirs`) with documentation tests.
pub fn crates_with_doctests(
    root: &Path,
    dirs: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    dirs.iter()
        .filter(|(dir, _)| dir_has_doctests(&root.join("crates").join(dir).join("src")))
        .map(|(_, name)| name.clone())
        .collect()
}

fn dir_has_doctests(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            // Binaries' documentation is not tested.
            name != "bin" && dir_has_doctests(&path)
        } else {
            name != "main.rs"
                && path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path).is_ok_and(|src| has_doctest(&src))
        }
    })
}

fn has_doctest(src: &str) -> bool {
    let mut open = false;
    for line in src.lines() {
        let line = line.trim_start();
        let Some(doc) = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//!"))
        else {
            continue;
        };
        let Some(info) = doc.trim().strip_prefix("```") else {
            continue;
        };
        if open {
            open = false;
            continue;
        }
        open = true;
        let tested = info
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .all(|t| {
                matches!(t, "rust" | "compile_fail" | "no_run" | "should_panic")
                    || t.starts_with("edition")
            });
        if tested {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_blocks_are_doctests_other_languages_are_not() {
        assert!(has_doctest("/// ```\n/// let x = 1;\n/// ```\nfn f() {}"));
        assert!(has_doctest(
            "//! ```compile_fail,edition2021\n//! x\n//! ```"
        ));
        assert!(!has_doctest(
            "/// ```text\n/// a\n/// ```\n/// ```ignore\n/// b\n/// ```"
        ));
        assert!(!has_doctest("/// ```ts\n/// const x = 1;\n/// ```"));
        // The closing fence of a `text` block is not the opening of a Rust one.
        assert!(!has_doctest("/// ```text\n/// a\n/// ```\nfn f() {}"));
        assert!(!has_doctest("// ```\nfn f() {}"));
    }

    #[test]
    fn finds_this_repositorys_doctests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let graph = crate::graph::Graph::load(&root).unwrap();
        let crates = crates_with_doctests(&root, &graph.dirs);
        assert!(crates.contains(&"velt_native".to_string()), "{crates:?}");
        assert!(!crates.contains(&"xtask".to_string()), "{crates:?}");
    }
}
