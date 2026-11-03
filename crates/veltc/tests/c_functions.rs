//! C functions (`declare c("lib") function`, docs/internals/design/c-ffi.md, #2187 step 1): a
//! package declares them, calls them in `unsafe` code and gives programs a safe API; the calls
//! go straight to the C library with the C calling convention, in debug (Cranelift) and release
//! (LLVM) builds, with `errno` captured for `lastOsError()`. A program can't declare one.

use std::path::Path;
use std::process::Output;

mod no_window;
#[allow(dead_code)]
// The `velt dev` session only: its HTTP and signal helpers serve tests/reload.rs.
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;

/// A package `cmath` with C declarations (`decls`) and the safe functions around them (`api`),
/// and a program depending on it whose `main` is `main`.
fn project(dir: &Path, lib: &str, main: &str) {
    let pkg = dir.join("cmath");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.vlt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cmath\", version: \"0.1.0\" };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.vlt"), lib).unwrap();
    std::fs::write(
        app.join("package.vlt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  dependencies: { cmath: { path: \"../cmath\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.vlt"), main).unwrap();
}

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

const LIB: &str = r#"declare c("c") function llabs(x: i64): i64;
declare c("c") function sqrt(x: f64): f64;
declare c("c") function cbrtf(x: f32): f32;
declare c("c") function abs(x: i32): i32;
declare c("c") function toupper(c: i32): i32;

export function absolute(x: i64): i64 {
  // SAFETY: `llabs` takes any `long long` but LLONG_MIN.
  return unsafe { llabs(x) };
}

export function root(x: f64): f64 {
  // SAFETY: `sqrt` takes any double.
  return unsafe { sqrt(x) };
}

export function cubeRoot(x: f32): f32 {
  // SAFETY: `cbrtf` takes any float.
  return unsafe { cbrtf(x) };
}

export function abs32(x: i32): i32 {
  // SAFETY: any int but INT_MIN.
  return unsafe { abs(x) };
}

export function upper(c: i32): i32 {
  // SAFETY: `c` is an unsigned char's value.
  return unsafe { toupper(c) };
}
"#;

const MAIN: &str = r#"import { absolute, root, cubeRoot, abs32, upper } from "cmath";

function main() {
  console.log(absolute(-42), root(2.25), cubeRoot(27), abs32(-7), String.fromCharCode(upper(97)));
}
"#;

#[test]
fn a_package_calls_c_functions_in_debug_and_release_builds() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "42 1.5 3 7 A\n",
            "velt {args:?}:\n{stderr}"
        );
    }
}

/// `errno` is captured right after a C call, so `lastOsError()` reads the call's error even
/// with code between them (here a string built and printed). Windows reads `GetLastError()`
/// there, which the C library's functions don't set: its capture of the CRT's `errno` comes
/// with std's move onto `declare c("c")` (#2187 step 2).
#[cfg(unix)]
#[test]
fn errno_is_captured_after_a_c_call() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { lastOsError } from \"velt/os\";\n\ndeclare c(\"c\") function close(fd: i32): i32;\n\n/// `close` of a descriptor that isn't open: -1, and errno EBADF.\nexport function closeBad(): [i32, i32] {\n  // SAFETY: -1 is never a descriptor, so nothing is closed.\n  const r = unsafe { close(-1) };\n  console.log(`closed: ${r}`);\n  return [r, lastOsError()];\n}\n",
        "import { closeBad } from \"cmath\";\n\nfunction main() {\n  const [r, err] = closeBad();\n  console.log(r, err === 9);\n}\n",
    );
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "closed: -1\n-1 true\n",
            "{stderr}"
        );
    }
}

/// `velt check` of the project: its output (stdout and stderr).
fn check(dir: &Path) -> String {
    let o = velt(dir, &["check"]);
    assert!(!o.status.success(), "expected errors");
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn a_file_without_a_package_cant_declare_c_functions() {
    let dir = test_dir::TestDir::new();
    let main = dir.path().join("main.vlt");
    std::fs::write(
        &main,
        "declare c(\"c\") function llabs(x: i64): i64;\n\nfunction main() {\n  // SAFETY: any long long but LLONG_MIN.\n  console.log(unsafe { llabs(-1) });\n}\n",
    )
    .unwrap();
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", main.to_str().unwrap()])
        .current_dir(dir.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains(
            "`declare c function llabs` is only allowed in a package: this file has no package.vlt"
        ),
        "{out}"
    );
}

#[test]
fn a_package_checks_its_own_c_declarations() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check"])
        .current_dir(dir.path().join("cmath"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// One C symbol has one signature: each declaration with another one is an error, once (however
/// often it is called), naming both declarations in Velt's types.
#[test]
fn one_symbol_has_one_signature() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "declare c(\"c\") function abs$i8(x: i8): i32;\ndeclare c(\"c\") function abs$u8(x: u8): i32;\ndeclare c(\"c\") function abs$i32(x: i32): i32;\ndeclare c(\"c\") function abs$u16(x: u16): i32;\ndeclare c(\"c\") function abs$b(x: bool): i32;\ndeclare c(\"c\") function abs$same(x: i8): i32;\n\nexport function f(): i32 {\n  // SAFETY: plain values.\n  return unsafe { abs$i8(-1) + abs$u8(2) + abs$i32(-3) + abs$u16(4) + abs$b(true) + abs$u8(5) + abs$same(6) };\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  console.log(f());\n}\n",
    );
    let o = velt(dir.path(), &["build"]);
    let out = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!o.status.success(), "{out}");
    assert_eq!(
        out.matches("C function `abs` has two signatures").count(),
        4,
        "{out}"
    );
    for d in [
        "abs$u8(u8): i32",
        "abs$i32(i32): i32",
        "abs$u16(u16): i32",
        "abs$b(bool): i32",
    ] {
        assert!(
            out.contains(&format!("`{d}` here, and `abs$i8(i8): i32` at ")),
            "{d}: {out}"
        );
    }
    assert!(!out.contains("abs$same"), "{out}");
    assert!(!out.contains("ICE") && !out.contains("[U8]"), "{out}");
}

/// On Windows `lastOsError()` reads `GetLastError()`, which the C library's functions don't set:
/// after a C call it is whatever the thread last set, not the call's `errno`. Pinned here so
/// the capture of the CRT's `errno` (#2187 step 2) has to change it.
#[cfg(windows)]
#[test]
fn windows_last_os_error_is_not_the_c_library_s_errno_yet() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { lastOsError } from \"velt/os\";\n\ndeclare c(\"c\") function SetLastError(code: u32): void;\ndeclare c(\"c\") function exp(x: f64): f64;\n\nexport function f(): i32 {\n  // SAFETY: plain values; exp overflows to infinity with ERANGE.\n  unsafe { SetLastError(5); exp(1000); }\n  return lastOsError();\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  console.log(f());\n}\n",
    );
    let o = velt(dir.path(), &["run"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&o.stdout), "5\n", "{stderr}");
}

#[test]
fn a_c_call_needs_unsafe_code() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "declare c(\"c\") function labs(x: i64): i64;\n\nexport function absolute(x: i64): i64 {\n  return labs(x);\n}\n",
        MAIN.lines().next().unwrap().replace(", root, cubeRoot, abs32, upper", "").as_str(),
    );
    let out = check(dir.path());
    assert!(
        out.contains("calling unsafe function `labs` needs an `unsafe` block or function"),
        "{out}"
    );
}

#[test]
fn what_cant_cross_and_unknown_libraries_are_errors() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "declare c(\"c\") function puts(s: string): i32;\ndeclare c(\"c\") async function later(): Promise<void>;\n\nexport const x = 1;\n",
        "import { x } from \"cmath\";\n\nfunction main() {\n  console.log(x);\n}\n",
    );
    let out = check(dir.path());
    for want in [
        "`string` cannot be passed to C: declare parameter `s` of `puts` as `StrBytes`",
        "C function `later` can't be `async` yet",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
    // Checked once the types are right (after sema).
    project(
        dir.path(),
        "declare c(\"sqlite3\") function sqlite3_libversion_number(): i32;\n\nexport const x = 1;\n",
        "import { x } from \"cmath\";\n\nfunction main() {\n  console.log(x);\n}\n",
    );
    let out = check(dir.path());
    assert!(out.contains("C library `sqlite3` is not known"), "{out}");
}

#[test]
fn webassembly_can_t_call_c_functions() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    let o = velt(dir.path(), &["build", "--target", "wasm32-wasip1"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("C function `llabs` is not available on wasm32-wasip1"),
        "{out}"
    );
}

/// `velt dev` (the JIT) calls C functions, and a C function no loaded library has is an error
/// naming its declaration, not a panic in the JIT.
#[test]
fn velt_dev_calls_c_functions_and_reports_a_missing_one() {
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { absolute, cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(absolute(-42), cubeRoot(27));\n}\n";
    project(dir.path(), LIB, main);
    std::fs::write(dir.path().join("app/main.vlt"), main).unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stdout(reload_support::Mark::default(), "42 3")
        .unwrap_or_else(|e| panic!("{e}"));
    drop(dev);
    std::fs::write(
        dir.path().join("cmath/src/lib.vlt"),
        "declare c(\"c\") function velt_no_such_c_function(x: i32): i32;\n\nexport function absolute(x: i64): i64 {\n  // SAFETY: none needed.\n  return unsafe { velt_no_such_c_function(1) } as i64 + x;\n}\n\nexport function cubeRoot(x: f32): f32 {\n  return x;\n}\n",
    )
    .unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stderr(
        reload_support::Mark::default(),
        "C function `velt_no_such_c_function` (declared at ",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let err = dev.stderr_since(reload_support::Mark::default()).join("\n");
    assert!(err.contains("lib.vlt:1:1)"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

const POINTERS: &str = include_str!("c_functions/pointers.vlt");

/// Pointers to C types (`declare c type`), `Ptr<T> | null` and `null`, arrays of numbers and
/// byte views as buffers, and strings as `StrBytes` (a lone surrogate passes as U+FFFD), in
/// debug and release builds.
#[test]
fn pointers_buffers_and_strings_cross() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        POINTERS,
        "import { written, same, filled, copied, roundTrip, equal } from \"cmath\";\n\nfunction main() {\n  console.log(written(\"abc\"), same(\"hello\", \"help\", 3), same(\"hello\", \"help\", 4));\n  console.log(filled(4).join(\",\"), copied([1.5, 2.5, -3]).join(\",\"), roundTrip([1, 2, 3, 250]).join(\",\"));\n  console.log(equal(\"héllo\", \"héllo\"), equal(\"a\" + String.fromCharCode(0xd800), \"a\\u{fffd}\"));\n  const long = \"a string far longer than an inline one, \".repeat(3);\n  console.log(equal(long, long), equal(long + \"x\", long + \"y\"), equal(`${long}é`, long + \"é\"));\n}\n",
    );
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "3 true false\n7,7,7,7 1.5,2.5,-3 1,2,3,250\ntrue true\ntrue false true\n",
            "velt {args:?}:\n{stderr}"
        );
    }
}

/// Only velt/ffi's `StrBytes` passes a string's bytes: a look-alike alias of `string` is a
/// `string` (and velt/ffi's hooks may not even be loaded).
#[test]
fn a_look_alike_str_bytes_is_a_string() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "type StrBytes = string;\n\ndeclare c(\"c\") function memcmp(a: StrBytes, b: StrBytes, n: usize): i32;\n\nexport function f(): i32 {\n  // SAFETY: both have a byte.\n  return unsafe { memcmp(\"a\", \"b\", 1) };\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  console.log(f());\n}\n",
    );
    let out = check(dir.path());
    assert!(out.contains("`string` cannot be passed to C"), "{out}");
    assert!(!out.contains("ICE"), "{out}");
}

#[test]
fn pointer_types_and_strings_are_checked() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { Ptr } from \"velt/ffi\";\n\ndeclare c type FILE;\n\ndeclare c(\"c\") function fclose(f: Ptr<FILE>): i32;\ndeclare c(\"c\") function malloc(n: usize): Ptr<u8> | null;\ndeclare c(\"c\") function puts(s: string): i32;\n\nexport function f(): void {\n  // SAFETY: only types are checked here.\n  unsafe {\n    const p = malloc(1);\n    if (p !== null) {\n      fclose(p);\n    }\n  }\n  const forged = Ptr.at<FILE>(8);\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  f();\n}\n",
    );
    let out = check(dir.path());
    for want in [
        "`string` cannot be passed to C: declare parameter `s` of `puts` as `StrBytes`",
        "`Ptr<u8>`",
        "calling unsafe",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}
