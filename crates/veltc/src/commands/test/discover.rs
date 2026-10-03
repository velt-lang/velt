//! Finding test files (`*.test.vlt`, `*.test.ts`, `*.test.tsx`) and the test functions inside
//! them.
//!
//! A test is a top-level `export function test_*()` (or `export async function test_*()`) with no
//! parameters and no generics. Tests must be exported because the generated harness imports them
//! from a separate module; a `test_*` function that is not exported (or takes parameters) is
//! reported and skipped.

use std::path::{Path, PathBuf};

use velt_syntax::ast;

/// How test files are named, for messages.
pub const TEST_FILES: &str = "*.test.vlt, *.test.ts, *.test.tsx";

/// Whether a file named `name` is a test file: a source module (`vpm::sources`) whose name
/// without the extension ends in `.test`.
pub fn is_test_file(name: &str) -> bool {
    vpm::sources::strip_source_extension(name).is_some_and(|stem| stem.ends_with(".test"))
}

/// Test files under `path` (recursively, skipping `target/`, `node_modules/`, hidden and
/// symlinked directories), sorted. An explicitly named file is used even without the `.test` suffix.
pub fn find_test_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    files_where(path, &is_test_file)
}

/// Source modules under `dir` (`.vlt`, `.ts`, `.tsx`; recursively, skipping `target/`,
/// `node_modules/`, hidden and symlinked directories), sorted.
pub fn source_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    files_where(dir, &vpm::sources::is_source_name)
}

/// Files under `dir` whose names satisfy `keep` (recursively, skipping `target/`,
/// `node_modules/`, hidden and symlinked directories), sorted. A symlinked directory can lead
/// back up (`src/up -> ..`), which would walk the package again, or forever.
fn files_where(dir: &Path, keep: &dyn Fn(&str) -> bool) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    collect(dir, keep, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, keep: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            let link = entry.file_type().is_ok_and(|t| t.is_symlink());
            if !link && name != "target" && name != "node_modules" && !name.starts_with('.') {
                collect(&path, keep, out)?;
            }
        } else if keep(&name) {
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
            "sub/c.test.ts",
            "sub/d.test.tsx",
            "sub/e.test.d.ts",
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
                tmp.path().join("sub/b.test.vlt"),
                tmp.path().join("sub/c.test.ts"),
                tmp.path().join("sub/d.test.tsx"),
            ]
        );
        assert_eq!(
            find_test_files(&tmp.path().join("main.vlt")).unwrap().len(),
            1
        );
        assert!(find_test_files(&tmp.path().join("nope")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_walked() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.test.vlt"), "").unwrap();
        std::fs::write(tmp.path().join("b.vlt"), "").unwrap();
        std::os::unix::fs::symlink("..", sub.join("up")).unwrap();
        std::os::unix::fs::symlink("b.vlt", tmp.path().join("link.vlt")).unwrap();
        let found = source_files(&sub).unwrap();
        assert_eq!(found, [sub.join("a.test.vlt")]);
        // A symlinked file is still a file.
        let found = source_files(tmp.path()).unwrap();
        let link = tmp.path().join("link.vlt");
        assert_eq!(
            found,
            [tmp.path().join("b.vlt"), link, sub.join("a.test.vlt")]
        );
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
