//! Finding test files (`*.test.vlt`) and the test functions inside them.
//!
//! A test is a top-level `export function test_*()` (or `export async function test_*()`) with no
//! parameters and no generics. Tests must be exported because the generated harness imports them
//! from a separate module; a `test_*` function that is not exported (or takes parameters) is
//! reported and skipped.

use std::path::{Path, PathBuf};

use velt_syntax::ast;

/// Suffix of test files.
pub const TEST_SUFFIX: &str = ".test.vlt";

/// Test files under `path` (recursively, skipping `target/` and hidden directories), sorted. An
/// explicitly named file is used even without the `.test.vlt` suffix.
pub fn find_test_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    let mut out = vec![];
    collect(path, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                collect(&path, out)?;
            }
        } else if name.ends_with(TEST_SUFFIX) {
            out.push(path);
        }
    }
    Ok(())
}

/// The tests of one module, in source order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestFunctions {
    /// Runnable tests.
    pub tests: Vec<String>,
    /// The runnable tests that are `async` (the harness awaits them).
    pub async_tests: Vec<String>,
    /// `test_*` functions that cannot run, with the reason.
    pub skipped: Vec<(String, &'static str)>,
}

/// Collect `test_*` functions from a parsed test module.
pub fn test_functions(module: &ast::Module) -> TestFunctions {
    let mut found = TestFunctions::default();
    for item in &module.items {
        let ast::ItemKind::Function(f) = &item.kind else {
            continue;
        };
        let name = &f.sig.name.name;
        if !name.starts_with("test_") {
            continue;
        }
        let problem = if !item.exported {
            Some("not exported (write `export function`)")
        } else if !f.sig.params.is_empty() || !f.sig.generics.is_empty() {
            Some("tests must not take parameters or be generic")
        } else {
            None
        };
        match problem {
            Some(reason) => found.skipped.push((name.clone(), reason)),
            None => {
                if f.sig.is_async {
                    found.async_tests.push(name.clone());
                }
                found.tests.push(name.clone());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    #[test]
    fn finds_test_files_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "a.test.vlt",
            "sub/b.test.vlt",
            "main.vlt",
            "target/c.test.vlt",
            ".git/d.test.vlt",
        ] {
            let p = tmp.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let found = find_test_files(tmp.path()).unwrap();
        assert_eq!(
            found,
            [
                tmp.path().join("a.test.vlt"),
                tmp.path().join("sub/b.test.vlt")
            ]
        );
        assert_eq!(
            find_test_files(&tmp.path().join("main.vlt")).unwrap().len(),
            1
        );
        assert!(find_test_files(&tmp.path().join("nope")).is_err());
    }

    #[test]
    fn collects_exported_parameterless_tests() {
        let src = "
            export function test_add() {}
            function test_hidden() {}
            export function test_param(x: i64) {}
            export function helper() {}
            export function test_sub(): void {}
            export async function test_io() {}
        ";
        let (module, diags) = velt_syntax::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let found = test_functions(&module);
        assert_eq!(found.tests, ["test_add", "test_sub", "test_io"]);
        assert_eq!(found.async_tests, ["test_io"]);
        let skipped: Vec<&str> = found.skipped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(skipped, ["test_hidden", "test_param"]);
    }
}
