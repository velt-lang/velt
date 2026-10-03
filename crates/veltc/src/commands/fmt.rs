//! `velt fmt [<file|dir>...] [--check]`: format source files in place with [`velt_fmt`].
//!
//! Without paths: the enclosing package's `package.vlt` and the `.vlt`, `.ts` and `.tsx` files of
//! its `src/` directory, or every `.vlt` file under the current directory when not in a package (a
//! TypeScript project's own files are left alone). Directories named on the command line contribute
//! their `.vlt` files, and the `.ts` and `.tsx` files under a package's `src/` and `tests/`
//! directories. Directories are searched recursively, skipping `target/`, `node_modules/` and
//! hidden directories. `--check` writes nothing and lists the files that would change. Exit code 1
//! if a file does not parse, cannot be read/written, or (with `--check`) is not formatted.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use velt_common::SourceMap;

/// `velt fmt`.
pub fn fmt_command(paths: &[PathBuf], check: bool) -> ExitCode {
    let files = match files_to_format(paths) {
        Ok(files) => files,
        Err(msg) => {
            crate::style::error(&msg);
            return ExitCode::from(1);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut failed = false;
    let mut unformatted = 0;
    for file in &files {
        let shown = file.strip_prefix(&cwd).unwrap_or(file);
        match format_file(file, check) {
            Ok(true) => {}
            Ok(false) => {
                unformatted += 1;
                // A closed pipe (`velt fmt --check | head`) is not an error worth panicking over.
                let _ = writeln!(std::io::stdout(), "{}", shown.display());
            }
            Err(msg) => {
                eprintln!("{msg}");
                failed = true;
            }
        }
    }
    if check && unformatted > 0 {
        eprintln!(
            "{unformatted} file{} would be reformatted (run `velt fmt`)",
            if unformatted == 1 { "" } else { "s" }
        );
    }
    if failed || (check && unformatted > 0) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Formats one file. `Ok(true)`: already formatted; `Ok(false)`: needed formatting (rewritten
/// unless `check`). Parse errors are returned rendered.
fn format_file(path: &Path, check: bool) -> Result<bool, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("error: cannot read `{}`: {e}", path.display()))?;
    let formatted = velt_fmt::format_source(&src).map_err(|diags| {
        let mut sm = SourceMap::new();
        sm.add(path, src.clone());
        let rendered: Vec<String> = diags.iter().map(|d| d.render(&sm)).collect();
        format!(
            "{}\nerror: `{}` was not formatted because it does not parse",
            rendered.join("\n"),
            path.display()
        )
    })?;
    if formatted == src {
        return Ok(true);
    }
    if !check {
        std::fs::write(path, formatted)
            .map_err(|e| format!("error: cannot write `{}`: {e}", path.display()))?;
    }
    Ok(false)
}

/// The files named by `paths` (directories expanded), or the default set when empty.
fn files_to_format(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    if paths.is_empty() {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        let (dir, keep): (PathBuf, fn(&Path) -> bool) = match vpm::manifest::find_package_root(&cwd)
        {
            Some(root) => {
                let manifest = root.join(vpm::manifest::MANIFEST_FILE);
                if manifest.is_file() {
                    out.push(manifest);
                } else {
                    // Only a `velt.toml`: report the migration like every other command.
                    vpm::Manifest::from_dir(&root)?;
                }
                (root.join("src"), vpm::sources::is_source_file)
            }
            None => (cwd, is_vlt),
        };
        collect(&dir, keep, &mut out)?;
    }
    for path in paths {
        if path.is_file() {
            out.push(path.clone());
        } else if path.is_dir() {
            collect(path, in_named_dir, &mut out)?;
        } else {
            return Err(format!("`{}` does not exist", path.display()));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn is_vlt(file: &Path) -> bool {
    file.extension().is_some_and(|e| e == "vlt")
}

/// Whether a file found in a directory named on the command line is formatted: a `.vlt` file, or
/// a `.ts` or `.tsx` file under its package's `src/` or `tests/` (elsewhere, and outside a
/// package, it may belong to a TypeScript project, as with `velt fmt` without paths).
fn in_named_dir(file: &Path) -> bool {
    if is_vlt(file) {
        return true;
    }
    if !vpm::sources::is_source_file(file) {
        return false;
    }
    let file = vpm::relpath::absolute(file);
    let Some(root) = file.parent().and_then(vpm::manifest::find_package_root) else {
        return false;
    };
    ["src", "tests"]
        .iter()
        .any(|d| file.starts_with(root.join(d)))
}

/// Files under `dir` that satisfy `keep`, recursively, skipping `target/`, `node_modules/` and
/// hidden directories.
fn collect(dir: &Path, keep: fn(&Path) -> bool, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && name != "node_modules" && !name.starts_with('.') {
                collect(&path, keep, out)?;
            }
        } else if keep(&path) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_in_place_and_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let messy = tmp.path().join("a.vlt");
        let clean = tmp.path().join("sub/b.vlt");
        let skipped = tmp.path().join("target/c.vlt");
        for p in [&messy, &clean, &skipped] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        }
        std::fs::write(&messy, "function main(){console.log('hi');}").unwrap();
        std::fs::write(&clean, "function f() {}\n").unwrap();
        std::fs::write(&skipped, "function   g(){}").unwrap();

        let ts = tmp.path().join("sub/c.ts");
        let tsx = tmp.path().join("sub/d.tsx");
        for (rel, src) in [
            ("sub/c.ts", ""),
            ("sub/d.tsx", ""),
            ("sub/e.d.ts", ""),
            ("node_modules/x/f.ts", ""),
        ] {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, src).unwrap();
        }

        // Outside a package, only `.vlt` files; in a package, `.ts` and `.tsx` files under `src/`
        // and `tests/` too.
        let files = files_to_format(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(files, [messy.clone(), clean.clone()]);
        let pkg = tmp.path().join("pkg");
        let in_pkg = [
            "src/a.ts",
            "src/b.d.ts",
            "tests/c.tsx",
            "scripts/d.ts",
            "e.vlt",
        ];
        for rel in in_pkg.iter().chain(&["package.vlt"]) {
            let p = pkg.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let files = files_to_format(std::slice::from_ref(&pkg)).unwrap();
        let expected = ["e.vlt", "package.vlt", "src/a.ts", "tests/c.tsx"].map(|f| pkg.join(f));
        assert_eq!(files, expected);
        std::fs::remove_dir_all(&pkg).unwrap();
        // Named explicitly, any file is formatted.
        let files = files_to_format(&[ts.clone(), tsx.clone()]).unwrap();
        assert_eq!(files, [ts, tsx]);
        assert_eq!(format_file(&messy, true), Ok(false));
        assert_eq!(format_file(&clean, true), Ok(true));
        assert_eq!(format_file(&messy, false), Ok(false));
        assert_eq!(
            std::fs::read_to_string(&messy).unwrap(),
            "function main() {\n  console.log(\"hi\");\n}\n"
        );
        assert_eq!(format_file(&messy, true), Ok(true));
    }

    #[test]
    fn refuses_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.vlt");
        std::fs::write(&bad, "function main( {").unwrap();
        let err = format_file(&bad, false).unwrap_err();
        assert!(err.contains("does not parse"), "{err}");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "function main( {");
        assert!(files_to_format(&[tmp.path().join("nope")]).is_err());
    }
}
