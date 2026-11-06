//! C structs (`declare c struct`, docs/internals/design/c-ffi.md "C structs", #2187): values
//! with C's layout that a package's code and its users build, copy and read, in debug
//! (Cranelift) and release (LLVM) builds; pointers to them cross to C. Passing one to C by value,
//! and pointer fields, come with later layers.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

/// A package `geom` with C structs and functions (`lib`), which calls the platform's C library
/// unchecked, and a program allowing it whose `main` module is `main`.
fn project(dir: &Path, lib: &str, main: &str) {
    let pkg = dir.join("geom");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"geom\", version: \"0.1.0\", native: { c: { c: { unchecked: true } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"geom\"],\n  dependencies: { geom: { path: \"../geom\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
}

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

const LIB: &str = r#"import { Ptr } from "velt/ffi";

export declare c struct CGPoint { x: f64; y: f64; }
export declare c struct CGSize { width: f64; height: f64; }
export declare c struct CGRect { origin: CGPoint; size: CGSize; }

/** One of each kind of field, with padding between them as C has it. */
export declare c struct Mixed { a: u8; b: i32; c: [f32, f32, f32]; ok: bool; d: f64; }

declare c("c") function free(p: Ptr<CGPoint> | null): void;

export function moved(r: CGRect, dx: f64): CGRect {
  return { origin: { x: r.origin.x + dx, y: r.origin.y }, size: r.size };
}

export function area(r: CGRect): f64 {
  return r.size.width * r.size.height;
}

export function freeNone(): i32 {
  // SAFETY: free(NULL) does nothing.
  unsafe { free(null); }
  return 1;
}
"#;

const MAIN: &str = r#"import { CGRect, Mixed, area, freeNone, moved } from "geom";

function main() {
  const r: CGRect = { origin: { x: 1, y: 2 }, size: { width: 3, height: 4 } };
  const s = r;
  const t = moved(s, 10);
  const u = { ...t, size: { width: 5, height: 6 } } as CGRect;
  const rs: CGRect[] = [r, t, u];
  const m: Mixed = { a: 7, b: -3, c: [1.5, 2.5, 3.5], ok: true, d: 0.25 };
  const later = () => r.origin.y + u.size.height;
  console.log(area(r), s.origin.x, t.origin.x, area(rs[2]), rs.length, later());
  console.log(m.a, m.b, m.c[2], m.ok, m.d, freeNone());
}
"#;

#[test]
fn c_structs_are_values_in_debug_and_release_builds() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "12 1 11 30 3 8\n7 -3 3.5 true 0.25 1\n",
            "velt {args:?}:\n{stderr}"
        );
    }
}

/// A C struct is held inline with C's layout: fields in order, each at its alignment, the size
/// rounded up to the largest alignment (`Mixed` is 32 bytes in C on every 64-bit target), and
/// never a counted box.
#[test]
fn c_structs_have_c_layout() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    let o = velt(dir.path(), &["build", "--emit", "vir"]);
    let vir = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let agg = |name: &str| {
        vir.lines()
            .find(|l| l.split_whitespace().nth(1) == Some(name))
            .unwrap_or_else(|| panic!("no aggregate `{name}` in:\n{vir}"))
            .split_once(' ')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    };
    assert_eq!(agg("CGPoint"), "CGPoint size=16 align=8 { f64@0, f64@8 }");
    let mixed = agg("Mixed");
    assert!(
        mixed.starts_with("Mixed size=32 align=8 { u8@0, i32@4, agg#")
            && mixed.ends_with("@8, bool@20, f64@24 }"),
        "{mixed}"
    );
    let rect = agg("CGRect");
    assert!(
        rect.starts_with("CGRect size=32 align=8 {") && rect.contains("@16 }"),
        "{rect}"
    );
}

#[test]
fn a_c_struct_crosses_to_c_by_value_only_in_a_later_layer() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "export declare c struct P { x: f64; y: f64; }\n\ndeclare c(\"c\") function take(p: P): void;\ndeclare c(\"c\") function give(): P;\n\nexport const x = 1;\n",
        "import { x } from \"geom\";\n\nfunction main() {\n  console.log(x);\n}\n",
    );
    let o = velt(dir.path(), &["check"]);
    let out = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    for want in [
        "C function `take` can't take or return C struct `P` by value yet",
        "C function `give` can't take or return C struct `P` by value yet",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}
