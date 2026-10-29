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

const REAL: &str = r#"import { IoResult, IoStatus } from "velt:io";

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

/// std's drivers reach the compiler with their signature records (what `check_std_driver`
/// checks std's declarations against).
#[test]
fn std_driver_records_reach_the_compiler() {
    let sigs = velt_rt_host::abi_symbols::driver_signatures();
    let get = |n: &str| sigs.iter().find(|(name, _)| *name == n).map(|(_, s)| *s);
    assert_eq!(
        get("velt_std_postgres__query_rows"),
        Some("async (u64,string,string,bool)->IoResult<u64>")
    );
    assert_eq!(get("velt_std_postgres__close"), Some("(u64)->void"));
    // And a program using std's driver compiles (its declarations match).
    let d = check_alone(
        "import { connect } from \"velt:postgres\";\n\nasync function main() {\n  const c = await connect(\"postgres://nobody@127.0.0.1:1/x\");\n  c.close();\n}\n",
    );
    assert_eq!(d, "", "{d}");
}

/// Diagnostics of lowering `src` (a program without packages) for `target`: what
/// `velt build --target` reports before code generation.
fn lower_for(src: &str, target: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main.vlt");
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

const PREAD: &str = "declare system function pread(fd: i32, buf: u8[], offset: i64): isize;\n";

/// A system function is bound per target: the declaration is checked against the table of the
/// target, a target without it accepts the declaration, and a call of it there is the error.
#[test]
fn system_functions_are_bound_per_target() {
    let call = format!(
        "{PREAD}function main() {{\n  const b: u8[] = [0];\n  console.log(pread(0, b, 0));\n}}\n"
    );
    for t in [
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
    ] {
        assert_eq!(lower_for(&call, t), "", "{t}");
    }
    let d = lower_for(&call, "wasm32-wasip1");
    assert!(
        d.contains("system function `pread` is not available on wasm32-wasip1"),
        "{d}"
    );
    assert!(d.contains("`main` calls it"), "{d}");
    assert!(d.contains("this target has no system functions"), "{d}");
    let d = lower_for(&call, "x86_64-pc-windows-msvc");
    assert!(
        d.contains("is available on aarch64-apple-darwin, x86_64-apple-darwin"),
        "{d}"
    );
    // A view argument (`b.subarray(..)`, `Intrinsic::Subarray`) on a target without the
    // function gives the same error, not an ICE from lowering a view that never ships (#1718).
    let view = format!(
        "{PREAD}function main() {{\n  const b: u8[] = [0, 0];\n  \
         console.log(pread(0, b.subarray(0, 1), 0));\n}}\n"
    );
    for t in ["wasm32-wasip1", "x86_64-pc-windows-msvc"] {
        let d = lower_for(&view, t);
        assert!(
            d.contains("system function `pread` is not available on"),
            "{t}: {d}"
        );
        assert!(!d.contains("ICE"), "{t}: {d}");
    }
    // Declared but never called: nothing to report anywhere.
    let unused = format!("{PREAD}function main() {{}}\n");
    assert_eq!(lower_for(&unused, "wasm32-wasip1"), "");
    // std's fd API is behind such declarations: a program that does not call it compiles.
    let fs = "import { readFileSync } from \"velt:fs\";\n\
              import { openSync } from \"velt:fs/fd\";\nfunction main() {\n  \
              console.log(readFileSync(\"x\"));\n}\n";
    assert_eq!(lower_for(fs, "wasm32-wasip1"), "");
    // A call of std's fd API where the target lacks its system functions is reported at the
    // program's call, once per call, as the std function it called.
    let fd = "import * as fs from \"velt:fs/fd\";\nfunction main() {\n  \
              const fd = fs.openSync(\"x\", \"r\");\n  fs.closeSync(fd);\n}\n";
    for t in ["x86_64-pc-windows-msvc", "wasm32-wasip1"] {
        let d = lower_for(fd, t);
        assert!(
            d.contains(&format!(
                "main.vlt:3:14: error: `openSync` (velt:fs/fd) is not available on {t}"
            )) && d.contains(&format!(
                "main.vlt:4:3: error: `closeSync` (velt:fs/fd) is not available on {t}"
            )) && d.contains("it calls the system function `open`")
                && d.matches("error:").count() == 2,
            "{d}"
        );
    }
}

/// `struct flock` has another layout on Linux than on macOS: a struct naming its fields fits
/// both, and Windows has its own functions (checked though untested there).
#[test]
fn struct_layouts_and_windows_declarations_check() {
    let lock =
        "struct Lock {\n  l_type: i16;\n  l_whence: i16;\n  l_start: i64;\n  l_len: i64;\n  \
                l_pid: i32;\n}\ndeclare system function fcntl(fd: i32, cmd: i32, lock: Lock): \
                i32;\nfunction main() {\n  const l: Lock = { l_type: 1, l_whence: 0, l_start: \
                0, l_len: 0, l_pid: 0 };\n  console.log(fcntl(0, 5, l));\n}\n";
    assert_eq!(lower_for(lock, "x86_64-unknown-linux-gnu"), "");
    assert_eq!(lower_for(lock, "aarch64-apple-darwin"), "");
    let win = "struct Overlapped {\n  Offset: u32;\n  OffsetHigh: u32;\n  InternalHigh: usize;\n}\n\
               declare system function ReadFile(file: usize, buf: u8[], ov: Overlapped): i32;\n\
               declare system function CreateFileW(name: readonly u16[], access: u32, share: u32, \
               disposition: u32, flags: u32): usize;\n\
               declare system function lastOsError(): i32;\n\
               function main() {\n  const n: u16[] = [120, 0];\n  const h = CreateFileW(n, 0x80000000, 1, 3, 0x80);\n  \
               const ov: Overlapped = { Offset: 0, OffsetHigh: 0, InternalHigh: 0 };\n  const b: u8[] = [0, 0];\n  \
               console.log(ReadFile(h, b, ov), ov.InternalHigh, lastOsError());\n}\n";
    // `ReadFile` waits for an `OVERLAPPED`: checked, but a call of it is an error for now.
    let d = lower_for(win, "x86_64-pc-windows-msvc");
    assert!(
        d.contains("system function `ReadFile` is not available on x86_64-pc-windows-msvc")
            && d.contains("no target can call it yet"),
        "{d}"
    );
    let open = win.replace("ReadFile(h, b, ov)", "h != 0");
    assert_eq!(lower_for(&open, "x86_64-pc-windows-msvc"), "");
    let bad = "declare system function ReadFile(file: u64, buf: u8[], n: u32): i32;\nfunction main() {}\n";
    let d = lower_for(bad, "x86_64-pc-windows-msvc");
    assert!(
        d.contains("`file` is a C `HANDLE`: declare it `usize`, not `u64`"),
        "{d}"
    );
}

/// The system functions' struct layouts of the host are the C library's (`libc`'s
/// definitions; velt_vir's table states them per target).
#[cfg(unix)]
#[test]
fn host_layouts_match_the_c_library() {
    use std::mem::{offset_of, size_of as sz};
    use velt_vir::system::{host_triple, table, Param, Struct};
    let host = host_triple();
    let layout = |f: &str, p: &str| {
        let has = |q: &Param| matches!(q, Param::Struct { name, .. } if *name == p);
        let f = table(&host)
            .iter()
            .find(|x| x.name == f && x.params.iter().any(has))
            .unwrap();
        f.params
            .iter()
            .find_map(|q| match q {
                Param::Struct { layout, .. } => Some(*layout),
                _ => None,
            })
            .unwrap()
    };
    let off = |s: &Struct, n: &str| s.fields.iter().find(|f| f.name == n).unwrap().offset as usize;
    let fl = layout("fcntl", "lock");
    assert_eq!(fl.size as usize, sz::<libc::flock>());
    assert_eq!(off(fl, "l_start"), offset_of!(libc::flock, l_start));
    assert_eq!(off(fl, "l_len"), offset_of!(libc::flock, l_len));
    assert_eq!(off(fl, "l_pid"), offset_of!(libc::flock, l_pid));
    assert_eq!(off(fl, "l_type"), offset_of!(libc::flock, l_type));
    assert_eq!(off(fl, "l_whence"), offset_of!(libc::flock, l_whence));
    let st = layout("fstat", "st");
    assert_eq!(st.size as usize, sz::<libc::stat>());
    assert_eq!(off(st, "st_size"), offset_of!(libc::stat, st_size));
    assert_eq!(off(st, "st_mode"), offset_of!(libc::stat, st_mode));
    assert_eq!(off(st, "st_ino"), offset_of!(libc::stat, st_ino));
    assert_eq!(off(st, "st_nlink"), offset_of!(libc::stat, st_nlink));
    assert_eq!(off(st, "st_blocks"), offset_of!(libc::stat, st_blocks));
    assert_eq!(off(st, "st_blksize"), offset_of!(libc::stat, st_blksize));
    assert_eq!(off(st, "st_mtime"), offset_of!(libc::stat, st_mtime));
    assert_eq!(
        off(st, "st_mtime_nsec"),
        offset_of!(libc::stat, st_mtime_nsec)
    );
    assert_eq!(off(st, "st_uid"), offset_of!(libc::stat, st_uid));
    assert_eq!(off(st, "st_rdev"), offset_of!(libc::stat, st_rdev));
}
