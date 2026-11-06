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
export declare c struct Mixed { a: u8; b: i32; ok: bool; d: f64; s: CGSize; }

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
  const m: Mixed = { a: 7, b: -3, ok: true, d: 0.25, s: { width: 3.5, height: 1 } };
  const later = () => r.origin.y + u.size.height;
  console.log(area(r), s.origin.x, t.origin.x, area(rs[2]), rs.length, later());
  console.log(m.a, m.b, m.s.width, m.ok, m.d, freeNone());
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
/// rounded up to the largest alignment (`Mixed` is 40 bytes in C on every 64-bit target), and
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
        mixed.starts_with("Mixed size=40 align=8 { u8@0, i32@4, bool@8, f64@16, agg#")
            && mixed.ends_with("@24 }"),
        "{mixed}"
    );
    let rect = agg("CGRect");
    assert!(
        rect.starts_with("CGRect size=32 align=8 {") && rect.contains("@16 }"),
        "{rect}"
    );
}

/// The layout of aggregate `name` in the VIR of `main` (a program on its own), after its id.
fn layout_in(dir: &Path, name: &str, main: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("main.velt"), main).unwrap();
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let vir = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    vir.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no aggregate `{name}` in:\n{vir}"))
        .split_once(' ')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default()
}

const BOXING: &str = "declare c struct P { x: f64; y: f64; }
declare c struct H { a: u8; p: P; }
const p0: P = { x: 1, y: 2 };
const h0: H = { a: 4, p: p0 };
";

/// Nothing elsewhere in the program changes a C struct's layout: not a conversion to `object`
/// or `any` (which count object types compared by identity), not `==` on `any` values, and not
/// an unrelated tuple written in place and copied (boxing/tuples.rs counts that tuple type).
/// Each would otherwise put a pointer where C has `p` inline: program 0 converts a struct to
/// `object`, program 1 compares two `any`s holding C structs, program 2 writes a copied tuple.
#[test]
fn nothing_else_in_the_program_boxes_a_c_struct() {
    let dir = test_dir::TestDir::new();
    let want = |l: &str| l.starts_with("H size=24 align=8 { u8@0, agg#") && l.ends_with("@8 }");
    for (k, body) in [
        "function main() { const o: object = p0; console.log(o !== null, h0.p.x); }",
        "function main() { const a: any = h0; const b: any = p0; console.log(a == b, h0.p.y); }",
        "function unrelated(): f64 { let t: [f64, f64] = [1, 2]; let u = t; u[0] = 9; console.log(t[0]); return u[0]; }
function main() { const o: object = h0; console.log(o !== null, h0.p.x, unrelated()); }",
    ]
    .into_iter()
    .enumerate()
    {
        let l = layout_in(&dir.path().join(k.to_string()), "H", &format!("{BOXING}{body}\n"));
        assert!(want(&l), "program {k}: {l}");
    }
}

/// A C struct has no identity: `===`, `==`, `Object.is`, `includes`, `indexOf`, `lastIndexOf`,
/// generic code and `any` all compare its content, as `Map` and `Set` keys do; its numbers
/// compare as each operation compares numbers (`NaN` is not `==` itself, but `Object.is` it).
#[test]
fn c_structs_compare_by_content_everywhere() {
    let dir = test_dir::TestDir::new();
    let main = r#"declare c struct P { x: f64; y: f64; }
declare c struct H { a: u8; p: P; }
const p0: P = { x: 1, y: 2 };
const p1: P = { x: 1, y: 2 };
const p2: P = { x: 3, y: 4 };
function eq<T>(a: T, b: T): boolean { return a == b; }
function same<T>(a: T): boolean { return a == a; }
function find<T>(xs: T[], v: T): number { for (let i = 0; i < xs.length; i++) { if (xs[i] === v) { return i; } } return -1; }
function main() {
  const xs: P[] = [p0, p2];
  console.log(eq(p0, p0), eq(p0, p1), eq(p0, p2), same(p0), find(xs, p2), find(xs, p1));
  console.log(xs.includes(p2), xs.indexOf(p2), xs.lastIndexOf(p1), xs.includes({ x: 9, y: 9 }));
  console.log(Object.is(p0, p0), Object.is(p0, p1), Object.is(p0, p2));
  const a: any = p0;
  const b: any = p0;
  console.log(a == b, a == (p1 as any), a == (p2 as any), p0 == p1, p0 === p2, p0 != p2);
  const h: H = { a: 1, p: p0 };
  const k: H = { a: 1, p: p1 };
  const o: P | null = p1;
  const t1: [P, i32] = [p0, 1];
  const t2: [P, i32] = [p1, 1];
  console.log(h == k, eq(h, { ...k, a: 2 }), o == p0, t1 == t2);
  const m = new Map<P, i32>();
  m.set(p0, 1);
  m.set(p1, 2);
  const s = new Set<any>();
  s.add(a);
  s.add(p1 as any);
  console.log(m.size, s.size);
  const n: P = { x: NaN, y: 0 };
  console.log(n == n, Object.is(n, n));
}
"#;
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(dir.path().join("main.velt"), main).unwrap();
    for args in [
        &["run", "main.velt"][..],
        &["run", "--release", "main.velt"][..],
    ] {
        let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(args)
            .current_dir(dir.path())
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "true true false true 1 0\ntrue 1 0 false\ntrue true false\ntrue true false true false true\ntrue false true true\n1 1\nfalse true\n",
            "velt {args:?}:\n{stderr}"
        );
    }
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

/// `any`, `Set<any>`, `Map<any>` and `includes` compare a C struct's content as the struct's
/// own type does: `===` by IEEE (`NaN` unequal, `-0` equal to `0`), `Object.is` by SameValue,
/// keys and `includes` by SameValueZero (`NaN` equal to every `NaN`, of either sign), for `f64`
/// and `f32` fields and nested structs alike. `indexOf` and `lastIndexOf` use `===`.
#[test]
fn any_keys_and_includes_compare_c_structs_by_their_number_rules() {
    let dir = test_dir::TestDir::new();
    let main = r#"declare c struct P { x: f64; y: f64; }
declare c struct F { x: f32; y: f32; }
declare c struct H { a: u8; p: P; }
function mk(x: f64, y: f64): P { return { x, y }; }
function mf(x: f32, y: f32): F { return { x, y }; }
function mh(x: f64): H { return { a: 1, p: mk(x, 1) }; }
function main() {
  const nan = 0 / 0;
  const nz = -0.0;
  const n: any = mk(nan, 1);
  const m: any = mk(nan, 1);
  const z: any = mk(0, 1);
  const w: any = mk(nz, 1);
  console.log(Object.is(n, m), Object.is(z, w), n === m, z === w, n == m);
  const st = new Set<any>();
  st.add(n);
  st.add(m);
  st.add(z);
  st.add(w);
  console.log(st.size, st.has(n), st.has(mk(nan, 1) as any), st.has(mk(nz, 1) as any));
  st.add(mk(-nan, 1) as any);
  console.log(st.size);
  const sp = new Set<P>();
  sp.add(mk(nan, 1));
  sp.add(mk(-nan, 1));
  sp.add(mk(0, 1));
  sp.add(mk(nz, 1));
  console.log(sp.size, sp.has(mk(nan, 1)));
  const mp = new Map<any, i32>();
  mp.set(n, 3);
  console.log(mp.get(mk(nan, 1) as any), mp.get(mk(-nan, 1) as any));
  const sf = new Set<any>();
  const f32nan = nan as f32;
  sf.add(mf(f32nan, 1) as any);
  sf.add(mf(f32nan, 1) as any);
  sf.add(mf(0, 1) as any);
  sf.add(mf(-0.0 as f32, 1) as any);
  const tf = new Set<F>();
  tf.add(mf(f32nan, 1));
  tf.add(mf(f32nan, 1));
  tf.add(mf(0, 1));
  tf.add(mf(-0.0 as f32, 1));
  console.log(sf.size, tf.size, Object.is(mf(f32nan, 1) as any, mf(f32nan, 1) as any));
  const sh = new Set<any>();
  sh.add(mh(nan) as any);
  sh.add(mh(nan) as any);
  sh.add(mh(0) as any);
  sh.add(mh(nz) as any);
  const th = new Set<H>();
  th.add(mh(nan));
  th.add(mh(nan));
  th.add(mh(0));
  th.add(mh(nz));
  console.log(sh.size, th.size, Object.is(mh(nz) as any, mh(0) as any));
  const ps: P[] = [mk(nan, 1), mk(0, 1)];
  console.log(ps.includes(mk(nan, 1)), ps.indexOf(mk(nan, 1)), ps.lastIndexOf(mk(nan, 1)));
  console.log(ps.includes(mk(nz, 1)), ps.indexOf(mk(nz, 1)));
  const xs: any[] = [n, z];
  console.log(xs.includes(m), xs.indexOf(m), xs.includes(w), xs.indexOf(w));
  const os: (P | null)[] = [mk(nan, 1), null];
  console.log(os.includes(mk(nan, 1)), os.includes(null), os.indexOf(mk(nan, 1)));
}
"#;
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(dir.path().join("main.velt"), main).unwrap();
    for args in [
        &["run", "main.velt"][..],
        &["run", "--release", "main.velt"][..],
    ] {
        let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(args)
            .current_dir(dir.path())
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "true false false true false\n2 true true true\n2\n2 true\n3 3\n2 2 true\n2 2 false\ntrue -1 -1\ntrue 1\ntrue -1 true 1\ntrue true -1\n",
            "velt {args:?}:\n{stderr}"
        );
    }
}
