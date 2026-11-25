//! `@modifies` on std's generated functions (velt_sema `decorators/modifies.rs`, #3543): the
//! parameters every instance modifies take their final mode where the function is declared.
//! std lists them; the misuse tests check a copy of std edited to use it wrongly. (A program
//! cannot write `@modifies`: velt/lang/intrinsics is std's own. A program's `toJSON` changing
//! its object, which raises an unlisted parameter, is the golden
//! `lang/json_to_json_changes_its_object`.)

mod no_window;
mod test_dir;

use std::path::Path;

/// A program whose `JSON.stringify` instantiates std's `writeJson`, which writes into its `w`.
const JSON_PROGRAM: &str = "interface P {\n  x: number;\n  name: string;\n}\n\nfunction main() {\n  const p: P = { x: 1, name: \"a\" };\n  console.log(JSON.stringify(p));\n}\n";

/// `velt check` of `src` with std from `std`, by units as `units` says (`None`: whole program).
fn check_with_std(std: &Path, units: Option<&str>, src: &str) -> std::process::Output {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_STD", std)
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    match units {
        Some(v) => cmd.env("VELT_UNITS", v),
        None => cmd.env_remove("VELT_UNITS"),
    };
    cmd.output().unwrap()
}

/// A copy of the repository's std in `to`, with `old` replaced by `new` in `file` (relative to
/// std), which must hold `old` exactly once.
fn edited_std(to: &Path, file: &str, old: &str, new: &str) {
    let repo_std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    copy_dir(&repo_std, to);
    let path = to.join(file);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches(old).count(), 1, "{file} holds `{old}` once");
    std::fs::write(&path, text.replace(old, new)).unwrap();
}

const WRITE_JSON: &str =
    "@modifies(\"w\")\nexport declare generated(\"velt/encoding/json/generate/write\") function writeJson<T>(";

/// std's own list is right: a JSON program checks clean in both modes.
#[test]
fn std_s_generated_functions_list_what_their_instances_modify() {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    for units in [None, Some("2")] {
        let out = check_with_std(&std, units, JSON_PROGRAM);
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(out.status.success(), "VELT_UNITS={units:?}:\n{said}");
    }
}

/// `@modifies` names a parameter of the function, and only on a `declare generated` one.
#[test]
fn modifies_names_a_parameter_of_a_generated_function() {
    let dir = test_dir::TestDir::new();
    let std = dir.path().join("std");
    let unknown = WRITE_JSON.replace("@modifies(\"w\")", "@modifies(\"w\", \"out\")");
    edited_std(&std, "encoding/json/generated.velt", WRITE_JSON, &unknown);
    let out = check_with_std(&std, None, JSON_PROGRAM);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        said.contains("`@modifies`: the function has no parameter `out`"),
        "{said}"
    );

    // On a std function with a body, which no generator writes.
    let dir = test_dir::TestDir::new();
    let std = dir.path().join("std");
    let plain = "function jsonCircular(): never {";
    let marked = "@modifies()\nfunction jsonCircular(): never {";
    edited_std(&std, "encoding/json/generated.velt", plain, marked);
    let out = check_with_std(&std, None, JSON_PROGRAM);
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        said.contains("`@modifies` is only for std's generated functions"),
        "{said}"
    );
}

/// Copy the directory `from` to `to`, with everything in it.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_dir(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
}
