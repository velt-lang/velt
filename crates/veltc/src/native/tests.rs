//! `check_declares` against a hand-made package graph: no cargo, no library, only the export
//! signatures a bundle's `native.toml` would list.

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

declare function demo_ok(): u64;
declare function demo_add(a: u64, b: i64): u64;
declare function demo_ad(a: u64): u64;
declare function demo_size(n: isize): u64;
declare function demo_cb(f: (x: i64) => i64): u64;
declare function velt_rt_os_platform(): string;

struct IoResult<T> {
  code: i32;
  message: string;
  value: T;
}
declare function demo_fake(): IoResult<u64>;

export function run(): u64 {
  return demo_ok() + real();
}
"#;

const REAL: &str = r#"import { IoResult } from "velt:io";

declare function demo_real(): IoResult<u64>;

export function real(): u64 {
  return demo_real().value;
}
"#;

const MAIN: &str = r#"import { run } from "demo";

declare function demo_ok(): u64;

function main() {
  console.log(run());
}
"#;

/// Diagnostics of checking `MAIN` in an app that depends on package `demo` with a library.
fn diagnostics() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let (app, demo) = (tmp.path().join("app"), tmp.path().join("demo"));
    write(
        &app.join("velt.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(&app.join("src/main.vlt"), MAIN);
    write(
        &demo.join("velt.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    write(&demo.join("src/lib.vlt"), LIB);
    write(&demo.join("src/real.vlt"), REAL);

    let exports = [
        ("demo_ok", "()->u64"),
        ("demo_add", "(u64,i32)->u64"),
        ("demo_size", "(i64)->u64"),
        ("demo_cb", "(i64)->u64"),
        ("demo_fake", "()->IoResult<u64>"),
        ("demo_real", "()->IoResult<u64>"),
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
    has("`declare` of `demo_add` does not match the native library of `demo 0.1.0`");
    has("the library exports `(u64,i32)->u64`");
    has("this declares     `(u64,i64)->u64`");
    // A name the library does not export, with a hint.
    has("`demo_ad` is not exported by the native library of `demo 0.1.0`");
    has("did you mean `demo_add`?");
    // Types that cannot cross the boundary.
    has("not `isize`");
    has("not `function`");
    // A look-alike of std's IoResult is not IoResult (the only result error is `demo_fake`'s).
    assert_eq!(d.matches("a native function returns").count(), 1, "{d}");
    assert!(d.contains("IoResult<T>` of one of those, not `"), "{d}");
    // Runtime functions are std's.
    has("`velt_rt_os_platform` is a runtime function");
    // Another package's export.
    has("`demo_ok` is exported by the native library of package `demo`; only that package may declare it");
    // The correct declarations (including std's IoResult) report nothing.
    assert!(!d.contains("demo_real"), "{d}");
    assert_eq!(d.matches("demo_ok").count(), 1, "{d}");
}
