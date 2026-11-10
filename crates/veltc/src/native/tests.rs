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

declare function velt_demo__ok(): u64;
declare function velt_demo__add(a: u64, b: i64): u64;
declare function velt_demo__ad(a: u64): u64;
declare function velt_demo__size(n: isize): u64;
declare function velt_demo__cb(f: (x: i64) => i64): u64;

struct IoResult<T> {
  code: i32;
  message: string;
  value: T;
}
declare function velt_demo__fake(): IoResult<u64>;

export function run(): u64 {
  return velt_demo__ok() + real();
}
"#;

const REAL: &str = r#"import { IoResult, IoStatus } from "velt/native";

declare function velt_demo__real(): IoResult<u64>;

struct Run {
  changes: i64;
  lastId: i64;
}
class Holder {
  v: i64 = 0;
}
struct Maybe {
  a?: i64;
}
declare function velt_demo__strs(xs: string[], r: Run): IoResult<Run[]>;
declare function velt_demo__renamed(r: Run): void;
declare function velt_demo__cls(h: Holder): void;
declare function velt_demo__opt(m: Maybe): void;
declare function velt_demo__status(): IoStatus;

export function real(): u64 {
  return velt_demo__real().value;
}
"#;

const MAIN: &str = r#"import { run } from "demo";

declare function velt_demo__ok(): u64;

function main() {
  console.log(run());
}
"#;

/// Diagnostics of checking `MAIN` in an app that depends on package `demo` with a library.
fn diagnostics() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let (app, demo) = (tmp.path().join("app"), tmp.path().join("demo"));
    write(
        &app.join("package.velt"),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };",
    );
    write(&app.join("src/main.velt"), MAIN);
    write(
        &demo.join("package.velt"),
        "export const pkg: Package = { name: \"demo\", version: \"0.1.0\" };",
    );
    write(&demo.join("src/lib.velt"), LIB);
    write(&demo.join("src/real.velt"), REAL);

    let exports = [
        ("velt_demo__ok", "()->u64"),
        ("velt_demo__add", "(u64,i32)->u64"),
        ("velt_demo__size", "(i64)->u64"),
        ("velt_demo__cb", "(i64)->u64"),
        ("velt_demo__fake", "()->IoResult<u64>"),
        ("velt_demo__real", "()->IoResult<u64>"),
        (
            "velt_demo__strs",
            "(string[],{changes:i64,lastId:i64})->IoResult<{changes:i64,lastId:i64}[]>",
        ),
        ("velt_demo__renamed", "({changes:i64,last:i64})->void"),
        ("velt_demo__cls", "(i64)->void"),
        ("velt_demo__opt", "(i64)->void"),
        ("velt_demo__status", "()->IoStatus"),
    ];
    let meta = NativeMeta {
        package: "demo".into(),
        version: "0.1.0".into(),
        target: velt_codegen_cl::host_triple(),
        abi: 1,
        shared: Some("shared/libvelt_native_demo.so".into()),
        import_lib: None,
        static_obj: None,
        system_libs: None,
        exports: exports
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect(),
        c: Default::default(),
    };
    let mut graph = PackageGraph::default();
    graph.add("app", &app, BTreeMap::from([("demo".into(), demo.clone())]));
    graph.add("demo", &demo, BTreeMap::new()).native = Some(NativeLib {
        dir: demo.join("bundle"),
        meta,
        origin: NativeOrigin::Prebuilt,
    });
    let opts = BuildOptions {
        input: app.join("src/main.velt"),
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
    has("`declare` of `velt_demo__add` does not match the native library of `demo 0.1.0`");
    has("the library exports `(u64,i32)->u64`");
    has("this declares     `(u64,i64)->u64`");
    // A name the library does not export, with a hint.
    has("`velt_demo__ad` is not exported by the native library of `demo 0.1.0`");
    has("did you mean `velt_demo__add`?");
    // Types that cannot cross the boundary.
    has("not `isize`");
    has("not `function`");
    // A look-alike of std's IoResult is not IoResult (the only result error is `velt_demo__fake`'s).
    assert_eq!(d.matches("a native function returns").count(), 1, "{d}");
    assert!(d.contains("`IoResult<T>` of a value (values: "), "{d}");
    // Arrays and structs cross by shape; a field with another name is another type.
    assert!(!d.contains("velt_demo__strs"), "{d}");
    // std's `IoStatus` is a struct, but crosses as `IoStatus`, not by its shape.
    assert!(!d.contains("velt_demo__status"), "{d}");
    has("this declares     `({changes:i64,lastId:i64})->void`");
    // Classes and optional fields do not cross.
    has("::Holder`");
    has("::Maybe`");
    // Another package's export, from a package without a library.
    has("`declare function velt_demo__ok` is not allowed: package `app` has no native library");
    has("`velt_demo__ok` is exported by the native library of package `demo`: import that package's API instead");
    // The correct declarations (including std's IoResult) report nothing.
    assert!(!d.contains("velt_demo__real"), "{d}");
    assert_eq!(d.matches("velt_demo__ok").count(), 2, "{d}");
}

/// Diagnostics of checking `src` as a program without packages.
fn check_alone(src: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main.velt");
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
    // std's own declarations (behind `velt/fs` and the prelude) are not affected.
    let d = check_alone(
        "import { readFile } from \"velt/fs\";\nfunction main() {\n  console.log(\"ok\");\n}\n",
    );
    assert_eq!(d, "");
}

/// Diagnostics of lowering `src` (a program without packages) for `target`: what
/// `velt build --target` reports before code generation.
fn lower_for(src: &str, target: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main.velt");
    write(&main, src);
    let opts = BuildOptions {
        input: main,
        target: Some(target.into()),
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile_to_vir(&mut sess, &opts) {
        Ok(_) => String::new(),
        Err(_) => sess.render_diagnostics(),
    }
}

/// `declare system function` is gone: an error naming `declare c("c")` (velt_syntax's parser).
#[test]
fn declare_system_is_an_error() {
    let src = "declare system function close(fd: i32): i32;\nfunction main() {\n  console.log(close(-1));\n}\n";
    let d = lower_for(src, "aarch64-apple-darwin");
    assert!(
        d.contains("`declare system function` was removed") && d.contains("declare c(\"c\")"),
        "{d}"
    );
}

/// std's fd API is `declare c("c")` on macOS and Linux behind the `#fd-system` variant of the
/// target (std/imports.velt), and fails with ENOSYS elsewhere: a program calling it compiles for
/// every target, and one that doesn't call it too.
#[test]
fn std_s_fd_api_compiles_for_every_target() {
    let fd = "import * as fs from \"velt/fs\";\nfunction main() {\n  \
              const fd = fs.openSync(\"x\", \"r\");\n  const b: u8[] = [0, 0];\n  \
              fs.readSync(fd, b, 0, 1, 0);\n  fs.writeSync(fd, \"x\");\n  fs.fsyncSync(fd);\n  \
              fs.fdatasyncSync(fd);\n  fs.ftruncateSync(fd, 1);\n  \
              console.log(fs.fstatSync(fd).size);\n  fs.closeSync(fd);\n  fs.unlinkSync(\"x\");\n}\n";
    let fs = "import { readFileSync } from \"velt/fs\";\nfunction main() {\n  \
              console.log(readFileSync(\"x\"));\n}\n";
    for t in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
        "wasm32-wasip1",
    ] {
        assert_eq!(lower_for(fd, t), "", "{t}");
        assert_eq!(lower_for(fs, t), "", "{t}");
    }
}

const NOW: &str = "declare host(\"env\") function now(): f64;\n";

/// A host function is a JS host's: declaring one is allowed anywhere, and lowering a call of it
/// for any other target is the error, naming the target and the caller. It is no native
/// library's `declare function`.
#[test]
fn host_functions_need_a_js_host() {
    let call = format!("{NOW}function main() {{\n  console.log(now());\n}}\n");
    assert_eq!(lower_for(&call, "wasm32-unknown-unknown"), "");
    for t in [
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "wasm32-wasip1",
    ] {
        let d = lower_for(&call, t);
        assert!(
            d.contains(&format!("host function `now` is not available on {t}")),
            "{t}: {d}"
        );
        assert!(d.contains("`main` calls it"), "{d}");
        assert!(
            d.contains("only a JS host (wasm32-unknown-unknown) provides host functions"),
            "{d}"
        );
        assert_eq!(d.matches("error:").count(), 1, "{d}");
    }
    let unused = format!("{NOW}function main() {{}}\n");
    assert_eq!(lower_for(&unused, "x86_64-unknown-linux-gnu"), "");
    // A view argument where the call is an error is no ICE either.
    let view = "declare host(\"env\") function send(b: readonly u8[]): void;\n\
                function main() {\n  const b: u8[] = [1, 2];\n  send(b.subarray(0, 1));\n}\n";
    let d = lower_for(view, "wasm32-wasip1");
    assert!(
        d.contains("host function `send` is not available") && !d.contains("ICE:"),
        "{d}"
    );
}

/// Two modules may declare one import with the same types; other types are an error, and so
/// are `u8[]` and `readonly u8[]` (one type, which crosses as a live view or as a copy).
#[test]
fn one_host_import_has_one_signature() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main.velt");
    // `other` declares `f(param): ret` and calls it; main declares `f(param2): ret2`.
    let lower = |other: &str, mine: &str, arg: &str| {
        // An async host function's promise is spawned (a floating one is an error).
        let call = |sig: &str| match sig.starts_with("async ") {
            true => format!("spawn(f({arg}))"),
            false => format!("f({arg})"),
        };
        let decl = |sig: &str| match sig.strip_prefix("async ") {
            Some(sig) => format!("export declare host(\"env\") async function f{sig};\n"),
            None => format!("export declare host(\"env\") function f{sig};\n"),
        };
        write(
            &tmp.path().join("other.velt"),
            &format!(
                "{}export function later(): void {{\n  const b: u8[] = [0];\n  {};\n}}\n",
                decl(other),
                call(other)
            ),
        );
        write(
            &main,
            &format!(
                "import {{ later }} from \"./other\";\n{}function main() {{\n  const b: u8[] = [0];\n  {};\n  later();\n}}\n",
                decl(mine).trim_start_matches("export "),
                call(mine)
            ),
        );
        let opts = BuildOptions {
            input: main.clone(),
            target: Some("wasm32-unknown-unknown".into()),
            ..Default::default()
        };
        let mut sess = Session::new();
        match driver::compile_to_vir(&mut sess, &opts) {
            Ok(_) => String::new(),
            Err(_) => sess.render_diagnostics(),
        }
    };
    let twice = "host function `f` of `env` is declared twice with different types";
    assert_eq!(lower("(x: f64): f64", "(y: f64): f64", "1"), "");
    assert_eq!(lower("(b: u8[]): void", "(c: u8[]): void", "b"), "");
    let d = lower("(x: f64): f64", "(x: f64): i32", "1");
    assert!(d.contains(twice) && !d.contains("readonly"), "{d}");
    assert_eq!(
        lower(
            "async (x: f64): Promise<u8>",
            "async (y: f64): Promise<u8>",
            "1"
        ),
        ""
    );
    let d = lower("async (x: f64): Promise<u8>", "(x: f64): u8", "1");
    assert!(d.contains(twice), "{d}");
    for (other, mine) in [
        ("(b: readonly u8[]): void", "(b: u8[]): void"),
        ("(b: u8[]): void", "(b: readonly u8[]): void"),
    ] {
        let d = lower(other, mine, "b");
        assert!(
            d.contains(twice) && d.contains("`readonly u8[]` differ"),
            "{d}"
        );
    }
}
