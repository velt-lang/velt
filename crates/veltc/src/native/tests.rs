//! `check_declares`: outside the standard library, a function without a body is a `declare c`
//! function (or a JS host's), never a plain `declare function`.

use std::collections::BTreeMap;
use std::path::Path;

use vpm::PackageGraph;

use crate::driver::{self, BuildError, BuildOptions, Session};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const LIB: &str = r#"declare function velt_demo__ok(): u64;

export function run(): u64 {
  return velt_demo__ok();
}
"#;

/// A package with native code may declare C functions, and nothing else: a plain `declare
/// function` (what a Rust crate's export was declared with, velt-lang/velt#2563) is an error.
#[test]
fn a_package_declares_c_functions_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, demo) = (tmp.path().join("app"), tmp.path().join("demo"));
    write(
        &app.join("package.velt"),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\", allowNative: [\"demo\"] };",
    );
    write(
        &app.join("src/main.velt"),
        "import { run } from \"demo\";\n\nfunction main() {\n  console.log(run());\n}\n",
    );
    write(
        &demo.join("package.velt"),
        "export const pkg: Package = { name: \"demo\", version: \"0.1.0\", native: { c: {} } };",
    );
    write(&demo.join("src/lib.velt"), LIB);
    let mut graph = PackageGraph::default();
    graph.add("app", &app, BTreeMap::from([("demo".into(), demo.clone())]));
    graph.add("demo", &demo, BTreeMap::new()).has_native_code = true;
    let opts = BuildOptions {
        input: app.join("src/main.velt"),
        packages: Some(graph),
        ..Default::default()
    };
    let mut sess = Session::new();
    let r = driver::check(&mut sess, &opts);
    assert!(matches!(r, Err(BuildError::Diagnostics)), "{r:?}");
    let d = sess.render_diagnostics();
    assert!(
        d.contains("`declare function velt_demo__ok` is not allowed outside the standard library"),
        "{d}"
    );
    assert!(d.contains("declare c(\"<library>\")"), "{d}");
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
        d.contains("`declare function free` is not allowed outside the standard library"),
        "{d}"
    );
    assert!(
        d.contains("to call C code, declare the function with"),
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

/// `declare system function` is not Velt: an ordinary error, not a program.
#[test]
fn declare_system_is_an_error() {
    let src = "declare system function close(fd: i32): i32;\nfunction main() {\n  console.log(close(-1));\n}\n";
    let d = lower_for(src, "aarch64-apple-darwin");
    assert!(d.contains("error") && !d.contains("ICE"), "{d}");
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
