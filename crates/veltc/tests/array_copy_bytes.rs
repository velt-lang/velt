//! std's `arrayCopyBytes<T>` (std/lang/intrinsics/array.vlt, std only) writes the bytes of a
//! `u8[]` as `T`s, so sema takes it only for a number type or `bool` as `T`: an element that owns
//! something or holds an address is a diagnostic, not an ICE in lowering. The intrinsic is
//! std-only, so a golden file cannot call it: std is copied to a temp directory with a probe
//! module added, and `velt` uses it through `$VELT_STD` (as inline_markers.rs does).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// A std with the module `velt/zz_bytes` (`probe`) added, and `main.vlt` calling `probe()`;
/// runs `velt <cmd> main.vlt`.
fn velt(probe: &str, cmd: &str) -> Output {
    let dir = test_dir::TestDir::new();
    let std: PathBuf = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::create_dir_all(std.join("zz_bytes")).unwrap();
    let probe = format!(
        "import {{ arrayCopyBytes, arraySetLen, arrayWithCapacity }} from \"velt/lang/intrinsics\";\n{probe}"
    );
    std::fs::write(std.join("zz_bytes/index.vlt"), probe).unwrap();
    let file = dir.path().join("main.vlt");
    std::fs::write(
        &file,
        "import { probe } from \"velt/zz_bytes\";\n\nfunction main() {\n  probe();\n}\n",
    )
    .unwrap();
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args([cmd, file.to_str().unwrap()])
        .env("VELT_STD", &std)
        .env("VELT_BUILD_CACHE", "off")
        .current_dir(dir.path())
        .output()
        .unwrap()
}

/// A probe calling `arrayCopyBytes` into a `T[]` for each `(name, T, n)` in `uses`, from the
/// bytes `b` (`c`, all 0 or 1, for `bool`), and logging each result with `show`.
fn probe(uses: &[(&str, &str, usize)], show: &str) -> String {
    let mut fns = String::new();
    let mut calls = String::new();
    for (name, t, n) in uses {
        fns.push_str(&format!(
            "function {name}(bytes: u8[]): {t}[] {{
  let out = arrayWithCapacity<{t}>({n});
  arraySetLen(out, {n});
  arrayCopyBytes(out, 0, bytes, 0, {n});
  return out;
}}
"
        ));
        let src = if *t == "bool" { "c" } else { "b" };
        calls.push_str(&format!("  console.log({name}({src}){show});\n"));
    }
    format!(
        "{fns}class Box {{
  x: number = 1;
}}
/** Probe. */
export function probe(): void {{
  const b: u8[] = [0, 60, 0, 64, 1, 0, 0, 0];
  const c: u8[] = [1, 0, 1, 1];
{calls}}}
"
    )
}

#[test]
fn number_and_bool_elements_copy() {
    let src = probe(
        &[
            ("f16s", "f16", 2),
            ("u32s", "u32", 2),
            ("u16s", "u16", 4),
            ("bools", "bool", 4),
        ],
        ".join(\",\")",
    );
    let out = velt(&src, "run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout,
        "1,2\n1073757184,1\n15360,16384,1,0\ntrue,false,true,true\n"
    );
}

#[test]
fn other_elements_are_a_diagnostic() {
    for (t, shown) in [
        ("string", "string"),
        ("Box", "Box"),
        ("any", "any"),
        ("(() => number)", "() => f64"),
    ] {
        let src = probe(&[("bad", t, 1)], ".length");
        let out = velt(&src, "check");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{t}: accepted");
        assert!(
            stderr.contains("`arrayCopyBytes` writes bytes into an array of numbers")
                && stderr.contains(&format!("found {shown}")),
            "{t}:\n{stderr}"
        );
        assert!(!stderr.contains("ICE"), "{t}:\n{stderr}");
    }
}
