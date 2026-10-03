//! `velt check --ts-compat` through the `velt` binary: text and JSON output, exit codes, which
//! files are linted (directories, the closure rule for relative imports, files failing the
//! check), and that every rule fixture of `velt_tscompat` is valid Velt.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

mod no_window;
mod test_dir;

fn velt(cwd: &Path, args: &[&str]) -> Output {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).expect("one JSON document")
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The `code`s of the diagnostics in a `--json` report (`null` for the check's own).
fn codes(report: &Value) -> Vec<String> {
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap_or("-").to_string())
        .collect()
}

/// The rules only ever see valid Velt, so each fixture must pass a plain `velt check`.
#[test]
fn every_rule_fixture_passes_velt_check() {
    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("../velt_tscompat/tests/cases");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&cases)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == "vlt" || e == "ts" || e == "tsx")
        })
        .collect();
    files.sort();
    assert!(files.len() > 10, "{files:?}");
    for file in files {
        let o = velt(&cases, &["check", file.to_str().unwrap()]);
        assert!(o.status.success(), "{}:\n{}", file.display(), stderr(&o));
    }
}

/// A fix must keep the code valid Velt: each `<case>.fixed` passes `velt check` under its
/// case's extension, next to the helpers (`_*` files) the cases import.
#[test]
fn every_fixed_case_passes_velt_check() {
    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("../velt_tscompat/tests/cases");
    let entries: Vec<PathBuf> = std::fs::read_dir(&cases)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    for helper in entries.iter().filter(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('_'))
    }) {
        std::fs::copy(helper, dir.join(helper.file_name().unwrap())).unwrap();
    }
    let mut fixed: Vec<&PathBuf> = entries
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "fixed"))
        .collect();
    fixed.sort();
    assert!(fixed.len() > 3, "{fixed:?}");
    for file in fixed {
        let ext = ["vlt", "ts", "tsx"]
            .into_iter()
            .find(|ext| file.with_extension(ext).is_file())
            .unwrap_or_else(|| panic!("{}: no case for it", file.display()));
        let name = file.with_extension(ext);
        let target = dir.join(name.file_name().unwrap());
        std::fs::copy(file, &target).unwrap();
        let o = velt(dir, &["check", target.to_str().unwrap()]);
        assert!(o.status.success(), "{}:\n{}", file.display(), stderr(&o));
    }
}

/// The `tsc` oracle's samples of what `tsc` rejects (tests/tscompat-oracle/rejected) are valid
/// Velt too, so they show what the lint sees. (Its behaviour sample, `declare function`, is
/// valid only in a package with a native library.)
#[test]
fn every_rejected_sample_passes_velt_check() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/tscompat-oracle/rejected");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert!(files.len() > 5, "{files:?}");
    for file in files {
        let o = velt(&dir, &["check", file.to_str().unwrap()]);
        assert!(o.status.success(), "{}:\n{}", file.display(), stderr(&o));
    }
}

#[test]
fn findings_print_as_diagnostics_with_their_code_and_fail() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "a.vlt",
        "export function f(x: f64): number {\n  return x;\n}\n",
    );
    let o = velt(dir, &["check", "--ts-compat", "a.vlt"]);
    let err = stderr(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("a.vlt:1:22: error: `f64` is not a TypeScript type"),
        "{err}"
    );
    assert!(err.contains("= note: ts-compat(velt-number-type)"), "{err}");
    // A plain check doesn't lint.
    assert!(velt(dir, &["check", "a.vlt"]).status.success());
}

#[test]
fn json_adds_code_and_fix() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "a.vlt",
        "export function f(x: f64): bool {\n  return x > 1;\n}\n",
    );
    let o = velt(dir, &["check", "--ts-compat", "a.vlt", "--json"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let report = json(&o);
    assert_eq!(codes(&report), ["velt-number-type", "bool-type"]);
    assert_eq!(report["errors"], 2);
    let d = &report["diagnostics"][1];
    assert_eq!(d["location"]["column"], 28);
    assert_eq!(d["fix"]["replacement"], "boolean");
    assert_eq!(d["fix"]["title"], "replace with `boolean`");
    assert_eq!(d["fix"]["location"]["endColumn"], 32);
    assert_eq!(
        d["notes"].as_array().unwrap().last().unwrap(),
        "ts-compat(bool-type)"
    );
    // The check's own diagnostics have both fields, as `null`.
    write(dir, "b.vlt", "export const x: number = \"a\";\n");
    let report = json(&velt(dir, &["check", "b.vlt", "--json"]));
    let d = &report["diagnostics"][0];
    assert!(d["code"].is_null() && d["fix"].is_null(), "{d}");
}

#[test]
fn clean_files_pass() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "m.ts", "export type P = { x: number; ok: boolean };\n");
    let o = velt(dir, &["check", "--ts-compat", "m.ts"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stderr(&o), "");
}

#[test]
fn relative_imports_must_stay_among_the_linted_files() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "shared/model.ts",
        "import { twice } from \"../util\";\nexport function four(): number {\n  return twice(2);\n}\n",
    );
    write(
        dir,
        "util.ts",
        "export function twice(x: number): number {\n  return x * 2;\n}\n",
    );
    let o = velt(dir, &["check", "--ts-compat", "shared", "--json"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let report = json(&o);
    assert_eq!(codes(&report), ["outside-import"]);
    let d = &report["diagnostics"][0];
    assert!(
        d["location"]["file"]
            .as_str()
            .unwrap()
            .ends_with("model.ts"),
        "{d}"
    );
    // The file to lint too, as named from the current directory.
    assert!(
        d["notes"][1]
            .as_str()
            .unwrap()
            .starts_with("lint `util.ts` too"),
        "{d}"
    );
    let o = velt(dir, &["check", "--ts-compat", "shared", "util.ts"]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn a_file_failing_the_check_is_not_linted() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "bad.vlt", "export const x: f64 = \"a\";\n");
    write(dir, "good.vlt", "export const y: f64 = 1.5;\n");
    let o = velt(dir, &["check", "--ts-compat", ".", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let report = json(&o);
    let files: Vec<(String, String)> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let file = d["location"]["file"].as_str().unwrap();
            let name = Path::new(file).file_name().unwrap().to_string_lossy();
            (
                name.into_owned(),
                d["code"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            ("bad.vlt".to_string(), "-".to_string()),
            ("good.vlt".to_string(), "velt-number-type".to_string())
        ]
    );
}

#[test]
fn paths_are_required_and_must_hold_sources() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let o = velt(dir, &["check", "--ts-compat"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("needs the files or directories to lint"),
        "{}",
        stderr(&o)
    );
    std::fs::create_dir(dir.join("empty")).unwrap();
    let o = velt(dir, &["check", "--ts-compat", "empty"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("has no `.vlt`, `.ts` or `.tsx` files"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn jsx_on_the_default_provider_is_reported() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "badge.tsx",
        "export function Badge(p: { n: string }): JSX.Element {\n  return <b>{p.n}</b>;\n}\n",
    );
    let report = json(&velt(dir, &["check", "--ts-compat", "badge.tsx", "--json"]));
    assert_eq!(codes(&report), ["jsx-provider"]);
    assert_eq!(report["diagnostics"][0]["location"]["line"], 2);
}

fn manifest(name: &str) -> String {
    format!(
        "import type {{ Package }} from \"velt:package\";\n\n\
         export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\" }};\n"
    )
}

#[test]
fn the_files_must_share_one_package() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "a/package.vlt", &manifest("alpha"));
    write(dir, "a/src/m.ts", "export const x: number = 1;\n");
    write(dir, "a/src/n.ts", "export const y: number = 2;\n");
    write(dir, "b/package.vlt", &manifest("beta"));
    write(dir, "b/src/m.ts", "export const x: number = 1;\n");
    write(dir, "loose.ts", "export const z: number = 3;\n");
    let o = velt(dir, &["check", "--ts-compat", "a/src/m.ts", "b/src/m.ts"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(
            "lint one package per run: `a/src/m.ts` is in package `alpha` (`a`), \
             `b/src/m.ts` in package `beta` (`b`)"
        ),
        "{}",
        stderr(&o)
    );
    let o = velt(dir, &["check", "--ts-compat", "a/src", "loose.ts"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("in no package"), "{}", stderr(&o));
    let o = velt(dir, &["check", "--ts-compat", "a/src/m.ts", "a/src/n.ts"]);
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn declaration_files_are_not_modules() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "types.d.ts", "export type T = number;\n");
    let o = velt(dir, &["check", "--ts-compat", "types.d.ts"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("declaration files (`.d.ts`) are not modules"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn files_with_the_same_module_path_are_reported() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "src/dup.vlt", "export function d() {}\n");
    write(dir, "src/dup.ts", "export function e() {}\n");
    let o = velt(dir, &["check", "--ts-compat", "src"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("have the same module path"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn findings_follow_the_check_after_a_blank_line() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "bad.vlt", "export const x: number = \"a\";\n");
    write(dir, "good.vlt", "export const y: f64 = 1.5;\n");
    let err = stderr(&velt(dir, &["check", "--ts-compat", "bad.vlt", "good.vlt"]));
    let (check, lint) = err.split_once("good.vlt:1:").expect(&err);
    assert!(
        check.ends_with("\n\n") && !check.ends_with("\n\n\n"),
        "{err}"
    );
    assert!(lint.contains("`f64` is not a TypeScript type"), "{err}");
}
