//! `check_declares` against a hand-made package graph: no cargo, no library, only the export
//! signatures a bundle's `native.json` would list.

use std::collections::BTreeMap;
use std::path::Path;

use vpm::native::{NativeLib, NativeMeta, NativeOrigin};
use vpm::PackageGraph;

use crate::driver::{self, BuildError, BuildOptions, Session};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const LIB: &str = r#"import { real } from "./real";

declare function velt_demo_ok(): u64;
declare function velt_demo_add(a: u64, b: i64): u64;
declare function velt_demo_ad(a: u64): u64;
declare function velt_demo_size(n: isize): u64;
declare function velt_demo_cb(f: (x: i64) => i64): u64;

struct IoResult<T> {
  code: i32;
  message: string;
  value: T;
}
declare function velt_demo_fake(): IoResult<u64>;

export function run(): u64 {
  return velt_demo_ok() + real();
}
"#;

const REAL: &str = r#"import { IoResult } from "velt:io";

declare function velt_demo_real(): IoResult<u64>;

export function real(): u64 {
  return velt_demo_real().value;
}
"#;

const MAIN: &str = r#"import { run } from "demo";

declare function velt_demo_ok(): u64;

function main() {
  console.log(run());
}
"#;

/// Diagnostics of checking `MAIN` in an app that depends on package `demo` with a library.
fn diagnostics() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let (app, demo) = (tmp.path().join("app"), tmp.path().join("demo"));
    write(
        &app.join("package.vlt"),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };",
    );
    write(&app.join("src/main.vlt"), MAIN);
    write(
        &demo.join("package.vlt"),
        "export const pkg: Package = { name: \"demo\", version: \"0.1.0\" };",
    );
    write(&demo.join("src/lib.vlt"), LIB);
    write(&demo.join("src/real.vlt"), REAL);

    let exports = [
        ("velt_demo_ok", "()->u64"),
        ("velt_demo_add", "(u64,i32)->u64"),
        ("velt_demo_size", "(i64)->u64"),
        ("velt_demo_cb", "(i64)->u64"),
        ("velt_demo_fake", "()->IoResult<u64>"),
        ("velt_demo_real", "()->IoResult<u64>"),
    ];
    let meta = NativeMeta {
        package: "demo".into(),
        version: "0.1.0".into(),
        target: velt_codegen_cl::host_triple(),
        abi: 1,
        shared: "shared/libvelt_native_demo.so".into(),
        import_lib: None,
        static_obj: None,
        exports: exports
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect(),
    };
    let mut graph = PackageGraph::default();
    graph.add("app", &app, BTreeMap::from([("demo".into(), demo.clone())]));
    graph.add("demo", &demo, BTreeMap::new()).native = Some(NativeLib {
        dir: demo.join("bundle"),
        meta,
        origin: NativeOrigin::Prebuilt,
    });
    let opts = BuildOptions {
        input: app.join("src/main.vlt"),
        packages: Some(graph),
        ..Default::default()
    };
    let mut sess = Session::new();
    let r = driver::check(&mut sess, &opts);
    assert!(matches!(r, Err(BuildError::Diagnostics)), "{r:?}");
    sess.render_diagnostics()
}

#[test]
fn declares_must_match_their_library_exactly() {
    let d = diagnostics();
    let has = |s: &str| assert!(d.contains(s), "missing `{s}` in:\n{d}");
    // A type that differs.
    has("`declare` of `velt_demo_add` does not match the native library of `demo 0.1.0`");
    has("the library exports `(u64,i32)->u64`");
    has("this declares     `(u64,i64)->u64`");
    // A name the library does not export, with a hint.
    has("`velt_demo_ad` is not exported by the native library of `demo 0.1.0`");
    has("did you mean `velt_demo_add`?");
    // Types that cannot cross the boundary.
    has("not `isize`");
    has("not `function`");
    // A look-alike of std's IoResult is not IoResult (the only result error is `velt_demo_fake`'s).
    assert_eq!(d.matches("a native function returns").count(), 1, "{d}");
    assert!(d.contains("IoResult<T>` of one of those, not `"), "{d}");
    // Another package's export, from a package without a library.
    has("`declare function velt_demo_ok` is not allowed: package `app` has no native library");
    has("`velt_demo_ok` is exported by the native library of package `demo`: import that package's API instead");
    // The correct declarations (including std's IoResult) report nothing.
    assert!(!d.contains("velt_demo_real"), "{d}");
    assert_eq!(d.matches("velt_demo_ok").count(), 2, "{d}");
}

/// Diagnostics of checking `src` as a program without packages.
fn check_alone(src: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main.vlt");
    write(&main, src);
    let opts = BuildOptions {
        input: main,
        ..Default::default()
    };
    let mut sess = Session::new();
    let r = driver::check(&mut sess, &opts);
    if r.is_ok() {
        return String::new();
    }
    sess.render_diagnostics()
}

#[test]
fn a_program_without_packages_declares_nothing() {
    let d =
        check_alone("declare function free(p: u64): void;\nfunction main() {\n  free(4096);\n}\n");
    assert!(
        d.contains("`declare function free` is not allowed: this program is not in a package with a native library"),
        "{d}"
    );
    assert!(
        d.contains("to call C code, give the package a native library"),
        "{d}"
    );
    let d = check_alone(
        "declare async function sleep_ms(n: u64): Promise<u64>;\nasync function main() {\n  await sleep_ms(1);\n}\n",
    );
    assert!(
        d.contains("`declare async function sleep_ms` is not allowed"),
        "{d}"
    );
    // std's own declarations (behind `velt:fs` and the prelude) are not affected.
    let d = check_alone(
        "import { readFile } from \"velt:fs\";\nfunction main() {\n  console.log(\"ok\");\n}\n",
    );
    assert_eq!(d, "");
}
