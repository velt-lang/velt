//! `velt check --ts-compat` through the `velt` binary: text and JSON output, exit codes, which
//! files are linted (directories, the closure rule for relative imports, files failing the
//! check), and that every rule fixture of `velt_tscompat` is valid Velt. Without paths it lints
//! the package's `tsCompat` folders.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

mod no_window;
mod runtime_support;
mod test_dir;
mod ts_compat_node;

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
/// case's extension, next to the helpers (`_*` files, and `_*` directories of JSX providers) the
/// cases import.
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
        copy_tree(helper, &dir.join(helper.file_name().unwrap()));
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

fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            copy_tree(&path, &to.join(path.file_name().unwrap()));
        }
    } else {
        std::fs::copy(from, to).unwrap();
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

/// The rules on types run in the command too; a warning alone doesn't fail it.
#[test]
fn typed_findings_and_warnings() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "a.ts",
        "export function has(m: Map<string, number>): boolean {\n  \
         return m.get(\"a\") === null;\n}\n",
    );
    write(
        dir,
        "b.ts",
        "export function size(s: string): number {\n  return s.length;\n}\n",
    );
    let o = velt(dir, &["check", "--ts-compat", "a.ts", "--json"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let report = json(&o);
    assert_eq!(codes(&report), ["strict-null-eq"]);
    assert_eq!(report["diagnostics"][0]["fix"]["replacement"], "==");
    let o = velt(dir, &["check", "--ts-compat", "b.ts", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let report = json(&o);
    assert_eq!(codes(&report), ["string-offsets"]);
    assert_eq!(report["diagnostics"][0]["severity"], "warning");
    assert_eq!(
        (report["errors"].as_u64(), report["warnings"].as_u64()),
        (Some(0), Some(1))
    );
    let o = velt(dir, &["check", "--ts-compat", "b.ts"]);
    assert!(o.status.success());
    assert!(
        stderr(&o).contains("b.ts:2:10: warning: `length` on a string counts UTF-8 bytes"),
        "{}",
        stderr(&o)
    );
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
fn paths_must_hold_sources() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
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

/// `tsc` ignores a pragma in a line comment; the fix writes it as a block comment, which Velt
/// reads the same way: the file keeps its provider (with `velt:jsx` it would get `jsx-provider`).
#[test]
fn a_line_comment_pragma_is_fixed_to_a_block_comment_with_the_same_provider() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "ui/jsx-runtime.vlt",
        "export * from \"velt:jsx/jsx-runtime\";\n",
    );
    let body =
        "export function Badge(p: { n: string }): JSX.Element {\n  return <b>{p.n}</b>;\n}\n";
    write(
        dir,
        "line.tsx",
        &format!("// @jsxImportSource ./ui\n{body}"),
    );
    write(
        dir,
        "block.tsx",
        &format!("/** @jsxImportSource ./ui */\n{body}"),
    );
    let report = json(&velt(dir, &["check", "--ts-compat", "line.tsx", "--json"]));
    assert_eq!(codes(&report), ["jsx-pragma-comment"]);
    let fix = &report["diagnostics"][0]["fix"];
    assert_eq!(
        fix["replacement"], "/** @jsxImportSource ./ui */",
        "{report}"
    );
    let o = velt(dir, &["check", "--ts-compat", "block.tsx"]);
    assert!(o.status.success(), "{}", stderr(&o));
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

/// A manifest named `app` with `tsCompat` set to `dirs` (a Velt array literal).
fn manifest_with_ts_compat(dirs: &str) -> String {
    format!(
        "import type {{ Package }} from \"velt:package\";\n\n\
         export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", tsCompat: {dirs} }};\n"
    )
}

/// The file names (without directories) and codes of a `--json` report's diagnostics.
fn files_and_codes(report: &Value) -> Vec<(String, String)> {
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let file = d["location"]["file"].as_str().unwrap_or("-");
            let name = Path::new(file).file_name().unwrap().to_string_lossy();
            (
                name.into_owned(),
                d["code"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect()
}

#[test]
fn without_paths_the_packages_ts_compat_folders_are_linted() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "package.vlt",
        &manifest_with_ts_compat("[\"src/models\", \"src/ui\"]"),
    );
    write(dir, "src/main.vlt", "export function main() {}\n");
    write(dir, "src/server.vlt", "export const port: number = 8080;\n");
    // Outside the folders: not linted.
    write(dir, "src/jobs.vlt", "export const big: i64 = 1;\n");
    write(dir, "src/models/user.ts", "export const age: i32 = 1;\n");
    write(
        dir,
        "src/models/deep/item.ts",
        "export const ok: bool = true;\n",
    );
    std::fs::create_dir_all(dir.join("src/ui")).unwrap();
    let o = velt(dir, &["check", "--ts-compat", "--json"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        files_and_codes(&json(&o)),
        [
            ("item.ts".to_string(), "bool-type".to_string()),
            ("user.ts".to_string(), "velt-number-type".to_string()),
        ]
    );
    // From a subdirectory, the files are named from there.
    let o = velt(&dir.join("src/models"), &["check", "--ts-compat"]);
    assert!(
        stderr(&o).contains("\nuser.ts:1:19: error:"),
        "{}",
        stderr(&o)
    );
    // A plain `velt check` of the package doesn't lint.
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // An import of a file outside the folders leaves the set.
    write(
        dir,
        "src/models/user.ts",
        "import { port } from \"../server\";\nexport const p: number = port;\n",
    );
    write(
        dir,
        "src/models/deep/item.ts",
        "export const ok: boolean = true;\n",
    );
    let report = json(&velt(dir, &["check", "--ts-compat", "--json"]));
    assert_eq!(
        files_and_codes(&report),
        [("user.ts".to_string(), "outside-import".to_string())]
    );
}

#[test]
fn a_package_nested_in_a_folder_is_left_out() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(dir, "package.vlt", &manifest_with_ts_compat("[\"shared\"]"));
    write(dir, "shared/user.ts", "export const age: number = 1;\n");
    // Another package's files, with findings of their own, and its manifest are not linted.
    write(dir, "shared/inner/package.vlt", &manifest("inner"));
    write(
        dir,
        "shared/inner/item.ts",
        "export const ok: bool = true;\n",
    );
    let o = velt(dir, &["check", "--ts-compat"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = velt(dir, &["check", "--ts-compat", "shared"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // Named itself, the nested package is linted (as its own package).
    let report = json(&velt(
        dir,
        &["check", "--ts-compat", "--json", "shared/inner"],
    ));
    assert_eq!(
        files_and_codes(&report),
        [("item.ts".to_string(), "bool-type".to_string())]
    );
}

#[test]
fn without_paths_a_package_must_list_existing_folders() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    // Outside a package.
    let o = velt(dir, &["check", "--ts-compat"]);
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(
        err.contains(
            "lints the folders a package's `tsCompat` lists, but there is no `package.vlt`"
        ),
        "{err}"
    );
    assert!(
        err.contains("or any parent directory. Or name the files or directories to lint: `velt check --ts-compat src/models`"),
        "{err}"
    );
    // A package without `tsCompat`.
    write(dir, "package.vlt", &manifest("app"));
    write(dir, "src/main.vlt", "export function main() {}\n");
    let err = stderr(&velt(dir, &["check", "--ts-compat"]));
    assert!(
        err.contains("package `app` has no `tsCompat` folders to lint: list them in package.vlt"),
        "{err}"
    );
    // A folder that isn't there; then one without sources.
    write(
        dir,
        "package.vlt",
        &manifest_with_ts_compat("[\"src/models\"]"),
    );
    let o = velt(dir, &["check", "--ts-compat", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        json(&o)["diagnostics"][0]["message"],
        "package `app`: `tsCompat` folder `src/models` does not exist"
    );
    std::fs::create_dir_all(dir.join("src/models")).unwrap();
    let err = stderr(&velt(dir, &["check", "--ts-compat"]));
    assert!(
        err.contains("the `tsCompat` folders have no `.vlt`, `.ts` or `.tsx` files to lint"),
        "{err}"
    );
    // Explicit paths ignore `tsCompat`.
    write(dir, "src/other.ts", "export const n: number = 1;\n");
    let o = velt(dir, &["check", "--ts-compat", "src/other.ts"]);
    assert!(o.status.success(), "{}", stderr(&o));
}
