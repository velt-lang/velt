//! A std module added in the middle of the check may add methods to a built-in vector type
//! (velt_loader's (a1) rule, velt/simd's `extend u8x16 { … }`, #3634). Where one would take over a
//! call checked before it (velt_sema `collect::vector_extends`), the module is refused and the
//! program checked again with it up front, as for a module the rule refuses; a clash no checked
//! call went through changes nothing and is served in place.

use std::path::Path;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");

const PROGRAM: &str = r#"import { i8x16 } from "velt/simd";

function main() {
  const v = i8x16.splat(1);
  console.log(v.sum());
  let n: i64 = 3;
  console.log(n ** 2);
}
"#;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
}

/// `velt build --timings` of the program against std at `std`: its success, stderr, and how many
/// checks it took (a `sema` line each).
fn build(dir: &Path, std: &Path) -> (bool, String, usize) {
    let o = no_window::command(VELT)
        .args(["build", "main.velt", "-o"])
        .arg(dir.join("main.bin"))
        .arg("--timings")
        .current_dir(dir)
        .env("VELT_STD", std)
        .env("VELT_BUILD_CACHE", "0")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    let checks = err.lines().filter(|l| l.starts_with("velt: sema ")).count();
    (o.status.success(), err, checks)
}

#[test]
fn a_mid_check_vector_method_may_not_shadow_one() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let std = root.join("std");
    copy_dir(&real, &std);
    let hook = std.join("lang/hooks/pow_int.velt");
    let original = std::fs::read_to_string(&hook).unwrap();

    // A method i8x16 doesn't have yet: served in place, one check.
    std::fs::write(
        &hook,
        format!("{original}\nextend i8x16 {{\n  zzNotInSimd(): i8 {{\n    return 0;\n  }}\n}}\n"),
    )
    .unwrap();
    let (ok, err, checks) = build(root, &std);
    assert!(ok, "{err}");
    assert_eq!(checks, 1, "{err}");

    // velt/simd's own `sum`: the program's `v.sum()` was checked against velt/simd's block, which
    // the new one would take over. Refused, checked again with it up front, where the two
    // equally specific blocks are ambiguous at the call, as when both load up front.
    std::fs::write(
        &hook,
        format!("{original}\nextend i8x16 {{\n  sum(): i8 {{\n    return 0;\n  }}\n}}\n"),
    )
    .unwrap();
    let (ok, err, checks) = build(root, &std);
    assert!(!ok, "a second `sum` of `i8x16` was accepted");
    assert!(!err.contains("ICE"), "{err}");
    assert!(
        err.contains("ambiguous extension method `sum` on `i8x16`"),
        "{err}"
    );
    assert_eq!(checks, 2, "checked again with the module up front: {err}");
}

/// A program's blanket `extend` whose method shares a name with one of velt/simd's (#3696's
/// review): velt/simd's block for the vector type is the more specific one, and no call on a
/// vector went through the program's block before velt/simd arrived, so the program builds with
/// one check and runs as on main, with or without `implements`.
#[test]
fn a_blanket_extend_of_the_programs_meets_velt_simd_in_the_middle_of_the_check() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let programs = [
        (
            "listen",
            "import { listenTls } from \"velt/net\";\n\nextend<T> T {\n  max(): string {\n    return \"blanket\";\n  }\n}\n\nasync function main() {\n  const n: i64 = 3;\n  console.log(n.max());\n  try {\n    const s = await listenTls(\"127.0.0.1:0\", { cert: \"c\", key: \"k\" });\n    console.log(s != null);\n  } catch (e) {\n    console.log(\"failed\");\n  }\n}\n",
        ),
        (
            "connect",
            "import { connectTls } from \"velt/net\";\n\ninterface I {\n  sum(): string;\n}\n\nextend<T> T implements I {\n  sum(): string {\n    return \"blanket\";\n  }\n}\n\nasync function main() {\n  const n: i64 = 3;\n  console.log(n.sum());\n  try {\n    const s = await connectTls(\"127.0.0.1:9\");\n    console.log(s != null);\n  } catch (e) {\n    console.log(\"failed\");\n  }\n}\n",
        ),
    ];
    for (name, src) in programs {
        std::fs::write(root.join("main.velt"), src).unwrap();
        let (ok, err, checks) = build(root, &real);
        assert!(ok, "{name}: {err}");
        assert_eq!(checks, 1, "{name}: {err}");
        let out = no_window::command(root.join("main.bin").to_str().unwrap())
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "blanket\nfailed\n",
            "{name}"
        );
    }
}

/// A vector held through `velt/arch/x86`'s `Vector256`, which loads only part of velt/simd: an
/// operator or method that only the rest of velt/simd gives was looked up and not found before
/// velt/simd arrived with the TLS code (#3696's second review). The module is refused and the
/// program checked again with it up front, as on main, wherever the use sits in the source.
#[test]
fn a_vector_lookup_nothing_answered_yet_refuses_the_block_that_would() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let programs = [
        ("operator before", P9, "6\nfailed\n"),
        ("operator after", P12, "failed\n6\n"),
        ("method before", P10, "106\nfailed\n"),
    ];
    for (name, src, want) in programs {
        std::fs::write(root.join("main.velt"), src).unwrap();
        let (ok, err, checks) = build(root, &real);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            checks, 2,
            "{name}: checked again with velt/simd up front: {err}"
        );
        let out = no_window::command(root.join("main.bin").to_str().unwrap())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}");
    }
}

/// `velt check` of a file with a syntax error (the recovered check, `driver/recover.rs`) after a
/// real takeover: checked again with velt/simd up front, so only the syntax error is reported,
/// not the abandoned check's findings in std.
#[test]
fn the_recovered_check_takes_a_late_refusal_too() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), P13).unwrap();
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let o = no_window::command(VELT)
        .args(["check", "main.velt"])
        .current_dir(root)
        .env("VELT_STD", &real)
        .env("VELT_BUILD_CACHE", "0")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(err.contains("expected identifier, found `{`"), "{err}");
    let errors = err.lines().filter(|l| l.contains("error:")).count();
    assert_eq!(errors, 1, "only the syntax error: {err}");
}

const P9: &str = r#"import { Vector256 } from "velt/arch/x86";
import { connectTls } from "velt/net";

async function main() {
  const bytes: u8[] = [];
  for (let i = 0; i < 32; i++) {
    bytes.push(i as u8);
  }
  const v = Vector256.loadU8(bytes).lower();
  const w = v + v;
  console.log(w.lane(3));
  try {
    const s = await connectTls("127.0.0.1:1", {});
    console.log(s != null);
  } catch (e) {
    console.log("failed");
  }
}
"#;

const P12: &str = r#"import { Vector256 } from "velt/arch/x86";
import { connectTls } from "velt/net";

async function tls() {
  try {
    const s = await connectTls("127.0.0.1:1", {});
    console.log(s != null);
  } catch (e) {
    console.log("failed");
  }
}

async function main() {
  await tls();
  later();
}

function later() {
  const bytes: u8[] = [];
  for (let i = 0; i < 32; i++) {
    bytes.push(i as u8);
  }
  const v = Vector256.loadU8(bytes).lower();
  const w = v + v;
  console.log(w.lane(3));
}
"#;

const P10: &str = r#"import { Vector256 } from "velt/arch/x86";
import { connectTls } from "velt/net";

async function main() {
  const bytes: u8[] = [];
  for (let i = 0; i < 32; i++) {
    bytes.push(i as u8);
  }
  const v = Vector256.loadU8(bytes).lower();
  console.log(v.aesenc(v).lane(0));
  try {
    const s = await connectTls("127.0.0.1:1", {});
    console.log(s != null);
  } catch (e) {
    console.log("failed");
  }
}
"#;

const P13: &str = r#"import { Vector256 } from "velt/arch/x86";
import { connectTls } from "velt/net";

extend<T> T {
  aesenc(rk: T): T {
    return rk;
  }
}

async function main() {
  const bytes: u8[] = [];
  for (let i = 0; i < 32; i++) {
    bytes.push(i as u8);
  }
  const v = Vector256.loadU8(bytes).lower();
  console.log(v.aesenc(v).lane(0));
  try {
    const s = await connectTls("127.0.0.1:1", {});
    console.log(s != null);
  } catch (e) {
    console.log("failed");
  }
  const bad: string = 42;
}

function broken( {
"#;
