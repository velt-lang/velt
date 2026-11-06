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
/// which says it has native code and calls the platform's C library unchecked (the header check
/// needs zig: tests/c_headers.rs), and a program depending on it, which
/// allows that code, whose `main` is `main`.
fn project(dir: &Path, lib: &str, main: &str) {
    project_with(dir, lib, main, NATIVE_C, "allowNative: [\"cmath\"], ");
}

/// `cmath`'s manifest fields of a package calling only the platform's C library.
const NATIVE_C: &str = ", native: { c: { c: { unchecked: true } } }";

/// [`project`] with `native` (`cmath`'s manifest fields after its version) and `allow` (the
/// app's).
fn project_with(dir: &Path, lib: &str, main: &str, native: &str, allow: &str) {
    let pkg = dir.join("cmath");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"cmath\", version: \"0.1.0\"{native} }};\n"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(
        app.join("package.velt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  {allow}dependencies: {{ cmath: {{ path: \"../cmath\" }} }},\n}};\n"),
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

/// A call whose declaration doesn't ask for `errno` leaves what `lastOsError()` reads alone: this
/// `close` fails with EBADF, and the slot still holds the failure of the call that asked.
#[cfg(unix)]
#[test]
fn only_a_declaration_that_asks_captures_errno() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { lastOsError } from \"velt/os\";\n\ndeclare c(\"c\", { errno: true }) function chdir$asks(path: CString): i32;\ndeclare c(\"c\") function close(fd: i32): i32;\n\nimport { CString } from \"velt/ffi\";\n\nexport function codes(): [i32, i32] {\n  // SAFETY: the path is NUL-terminated and borrowed for the call.\n  unsafe { chdir$asks(new CString(\"/no/such/directory\")) };\n  const asked = lastOsError();\n  // SAFETY: closing a descriptor that isn't open only fails.\n  unsafe { close(-1) };\n  return [asked, lastOsError()];\n}\n",
        "import { codes } from \"cmath\";\n\nfunction main() {\n  const [asked, after] = codes();\n  console.log(asked != 0, asked == after);\n}\n",
    );
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "true true\n",
            "{stderr}"
        );
    }
}

/// `errno` is captured right after a C call that asks for it, so `lastOsError()` reads the call's
/// error even with code between them (here a string built and printed). Windows reads
/// `GetLastError()` there, which the C library's functions don't set: its capture of the CRT's
/// `errno` comes with std's move onto `declare c("c")` (#2187 step 2).
#[cfg(unix)]
#[test]
fn errno_is_captured_after_a_c_call() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { lastOsError } from \"velt/os\";\n\ndeclare c(\"c\", { errno: true }) function close(fd: i32): i32;\n\n/// `close` of a descriptor that isn't open: -1, and errno EBADF.\nexport function closeBad(): [i32, i32] {\n  // SAFETY: -1 is never a descriptor, so nothing is closed.\n  const r = unsafe { close(-1) };\n  console.log(`closed: ${r}`);\n  return [r, lastOsError()];\n}\n",
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
    let main = dir.path().join("main.velt");
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
            "`declare c function llabs` is only allowed in a package: this file has no package.velt"
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
    assert!(
        out.contains(
            "C library `sqlite3` is not in package.velt's `native.c`: package `cmath` lists `c`"
        ),
        "{out}"
    );
}

/// A package declaring C functions says it has native code (`native.c`), and a program needs
/// `allowNative` to install it: one error each, naming what to add, and none once it's there.
#[test]
fn a_program_allows_a_package_s_c_declarations() {
    let dir = test_dir::TestDir::new();
    project_with(dir.path(), LIB, MAIN, "", "allowNative: [\"cmath\"], ");
    let out = check(dir.path());
    let unsaid = "package `cmath` declares C function `llabs`, but its package.velt doesn't say it has native code";
    assert!(out.contains(unsaid), "{out}");
    assert_eq!(
        out.matches("doesn't say it has native code").count(),
        1,
        "{out}"
    );
    assert!(
        out.contains("`native: { c: { c: { headers: [\"<string.h>\"] } } }`"),
        "{out}"
    );
    project_with(dir.path(), LIB, MAIN, NATIVE_C, "");
    let out = check(dir.path());
    let refusal = "package `cmath` has native code, which runs with no safety net";
    assert!(out.contains(refusal), "{out}");
    assert!(out.contains("allowNative: [\"cmath\"]"), "{out}");
    project(dir.path(), LIB, MAIN);
    let o = velt(dir.path(), &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

const LIBM: &str = r#"declare c("m") function cbrt(x: f64): f64;

export function cubeRoot(x: f64): f64 {
  // SAFETY: `cbrt` takes any double.
  return unsafe { cbrt(x) };
}
"#;

/// A C library of the package's own (`native.c`), linked per target: libm (in the C library on
/// macOS, which links it anyway), in debug and release builds and under `velt dev`.
#[cfg(unix)]
#[test]
fn a_package_s_c_library_is_linked_per_target() {
    let dir = test_dir::TestDir::new();
    // Rounded: glibc's `cbrt(27)` is 3.0000000000000004. What's tested is that libm links.
    let main = "import { cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(Math.round(cubeRoot(27)));\n}\n";
    let native =
        ", native: { c: { m: { system: { linux: \"m\", darwin: \"m\" }, unchecked: true } } }";
    project_with(dir.path(), LIBM, main, native, "allowNative: [\"cmath\"], ");
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "3\n", "velt {args:?}");
    }
    reload_support::build_runtime();
    std::fs::write(dir.path().join("app/main.velt"), main).unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stdout(reload_support::Mark::default(), "3")
        .unwrap_or_else(|e| panic!("{e}"));
}

/// A build with nothing changed links again from its stamp, without the check that finds the C
/// libraries, and they must still reach the linker (on macOS libm is part of the C library, so only
/// Linux shows it).
#[cfg(unix)]
#[test]
fn an_unchanged_build_keeps_its_c_libraries() {
    let dir = test_dir::TestDir::new();
    let main = "import { cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(Math.round(cubeRoot(27)));\n}\n";
    let native =
        ", native: { c: { m: { system: { linux: \"m\", darwin: \"m\" }, unchecked: true } } }";
    project_with(dir.path(), LIBM, main, native, "allowNative: [\"cmath\"], ");
    for round in ["first", "unchanged"] {
        let o = velt(dir.path(), &["run"]);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{round} build failed:\n{stderr}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "3\n", "{round}");
    }
}

/// A C library a package declares functions of lists its headers (tests/c_headers.rs checks
/// them; that needs zig) or says `unchecked: true`; one doing neither is an error, the platform's
/// `c` without an entry too.
#[test]
fn a_c_library_lists_its_headers_or_says_it_has_none() {
    let dir = test_dir::TestDir::new();
    let main = "import { cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(cubeRoot(27));\n}\n";
    let allow = "allowNative: [\"cmath\"], ";
    let native = ", native: { c: { m: { system: { default: \"m\" } } } }";
    project_with(dir.path(), LIBM, main, native, allow);
    let out = check(dir.path());
    assert!(
        out.contains("error: package `cmath` declares functions of C library `m`, but its package.velt lists no headers to check them against"),
        "{out}"
    );
    assert!(
        out.contains("note: list them: `native: { c: { m: { headers: [\"native/<library>.h\"] } } }`; a library without headers (a binary-only SDK) says `unchecked: true` instead"),
        "{out}"
    );
    assert_eq!(out.matches("error:").count(), 1, "{out}");
    project_with(dir.path(), LIB, MAIN, ", native: { c: {} }", allow);
    let out = check(dir.path());
    assert!(
        out.contains("declares functions of C library `c`, but its package.velt lists no headers")
            && out.contains("headers: [\"<string.h>\"]"),
        "{out}"
    );
    let native = ", native: { c: { m: { system: { default: \"m\" }, unchecked: true } } }";
    project_with(dir.path(), LIBM, main, native, allow);
    let o = velt(dir.path(), &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A library a package lists without an entry for the build's target, and system library names
/// that aren't library names, are errors.
#[test]
fn c_libraries_need_an_entry_for_the_target() {
    let dir = test_dir::TestDir::new();
    let main = "import { cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(cubeRoot(27));\n}\n";
    // An entry for another OS only.
    let other = if cfg!(windows) { "linux" } else { "win32" };
    let native = format!(", native: {{ c: {{ m: {{ system: {{ {other}: \"m\" }} }} }} }}");
    let native = native.as_str();
    project_with(dir.path(), LIBM, main, native, "allowNative: [\"cmath\"], ");
    let out = check(dir.path());
    assert!(
        out.contains("C library `m` of package `cmath` has no `system` entry for target `"),
        "{out}"
    );
    let native = ", native: { c: { m: { system: { default: \"-Wl,-rpath,/tmp\" } } } }";
    project_with(dir.path(), LIBM, main, native, "allowNative: [\"cmath\"], ");
    let out = check(dir.path());
    assert!(
        out.contains("`-Wl,-rpath,/tmp` is not a system library"),
        "{out}"
    );
    let native = ", native: { c: { m: { system: { default: \"framework:Accelerate\" } } } }";
    project_with(dir.path(), LIBM, main, native, "allowNative: [\"cmath\"], ");
    let out = check(dir.path());
    assert!(out.contains("a framework is macOS-only"), "{out}");
}

/// A package can't call C on WebAssembly: on wasm32-wasip1 only std calls wasi-libc (a
/// package's declarations can't be checked against WASI's headers yet,
/// docs/internals/design/wasi-libc.md), and the JS host has no C library at all.
#[test]
fn a_package_can_t_call_c_functions_on_webassembly() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN);
    for (target, note) in [
        (
            "wasm32-wasip1",
            "C functions in packages need a native target on wasm32-wasip1 until their headers can be checked",
        ),
        ("wasm32-unknown-unknown", "the JS host has no C library"),
    ] {
        let o = velt(dir.path(), &["build", "--target", target]);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(!o.status.success(), "{target}: {out}");
        assert!(
            out.contains(&format!("C function `llabs` is not available on {target}")),
            "{target}: {out}"
        );
        assert!(out.contains(note), "{target}: {out}");
    }
}

/// `velt dev` (the JIT) calls C functions, and a C function no loaded library has is an error
/// naming its declaration, not a panic in the JIT.
#[test]
fn velt_dev_calls_c_functions_and_reports_a_missing_one() {
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { absolute, cubeRoot } from \"cmath\";\n\nfunction main() {\n  console.log(absolute(-42), cubeRoot(27));\n}\n";
    project(dir.path(), LIB, main);
    std::fs::write(dir.path().join("app/main.velt"), main).unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stdout(reload_support::Mark::default(), "42 3")
        .unwrap_or_else(|e| panic!("{e}"));
    drop(dev);
    std::fs::write(
        dir.path().join("cmath/src/lib.velt"),
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
    assert!(err.contains("lib.velt:1:1)"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

const POINTERS: &str = include_str!("c_functions/pointers.velt");

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

/// A `declare c type` exists only behind a `Ptr`: `new` on it is an error, in its package and
/// in a program importing it; pointers to different C types never convert implicitly; a
/// read-only array passed for a buffer C writes names the C function.
#[test]
fn c_types_are_only_behind_pointers() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { Ptr } from \"velt/ffi\";\n\nexport declare c type FILE;\n\ndeclare c(\"c\") function malloc(n: usize): Ptr<u8> | null;\ndeclare c(\"c\") function fclose(f: Ptr<FILE>): i32;\ndeclare c(\"c\") function memset(dst: u8[], c: i32, n: usize): Ptr<u8>;\n\nconst FIXED: readonly u8[] = [1, 2];\n\nexport function f(): void {\n  const mine = new FILE();\n  // SAFETY: only types are checked here.\n  unsafe {\n    const p = malloc(1);\n    if (p !== null) {\n      fclose(p);\n    }\n    memset(FIXED, 0, 2);\n  }\n}\n",
        "import { f, FILE } from \"cmath\";\n\nfunction main() {\n  const theirs = new FILE();\n  f();\n}\n",
    );
    let out = check(dir.path());
    assert_eq!(
        out.matches("`FILE` is a C type: it exists only behind a `Ptr<FILE>`")
            .count(),
        2,
        "{out}"
    );
    assert!(out.contains("a `Ptr<u8>` is not a `Ptr<FILE>`"), "{out}");
    assert!(out.contains("`Ptr.at<FILE>(Ptr.addressOf(p))`"), "{out}");
    assert!(out.contains("a buffer the C function writes"), "{out}");
}

/// An empty array reaches C as a non-null address (C's `memset` and `memcpy` given NULL are
/// undefined behaviour even for 0 bytes): a `[]` literal holds a null data pointer, an emptied
/// array a dangling one. A C function declared to return `Ptr<T>` that returns NULL stops the
/// program, naming it.
#[test]
fn empty_arrays_are_never_null_and_a_null_ptr_result_is_caught() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { Ptr } from \"velt/ffi\";\n\ndeclare c(\"c\") function memset(dst: u8[], c: i32, n: usize): Ptr<u8> | null;\ndeclare c(\"c\") function getenv$wrong(name: u8[]): Ptr<u8>;\n\nexport function nonNull(): [bool, bool] {\n  const e: u8[] = [];\n  const g: u8[] = [1];\n  g.pop();\n  // SAFETY: 0 bytes are written.\n  return unsafe { [memset(e, 0, 0) !== null, memset(g, 0, 0) !== null] };\n}\n\nexport function wrong(): void {\n  const name: u8[] = [0x56, 0x45, 0x4c, 0x54, 0x5f, 0x4e, 0x4f, 0x53, 0x55, 0x43, 0x48, 0];\n  // SAFETY: the name is NUL-terminated; the declaration's `Ptr<u8>` result is wrong on purpose.\n  unsafe { getenv$wrong(name); }\n}\n",
        "import { nonNull, wrong } from \"cmath\";\n\nfunction main() {\n  console.log(nonNull());\n  wrong();\n}\n",
    );
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "[ true, true ]\n",
            "{stderr}"
        );
        assert!(!o.status.success(), "{stderr}");
        assert!(
            stderr.contains("C function `getenv` returned NULL, but its declaration's result is `Ptr<T>`: declare it `Ptr<T> | null`"),
            "{stderr}"
        );
    }
}

const RAW: &str = include_str!("c_functions/raw.velt");

/// `CString` (refused with a NUL inside), `Out<T>` and `OutPtr<T>` written by C, and C memory
/// read back with `readCString`, `readUtf8` and `readBytes`, in debug and release builds.
#[test]
fn c_strings_out_parameters_and_reads() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        RAW,
        "import { length, parse, env, roundTrip, exponent, negativeHex } from \"cmath\";\n\nfunction main() {\n  console.log(length(\"héllo\"), parse(\"42abc\"), env(\"PATH\") !== null, env(\"NO_SUCH_VAR_X\") === null, negativeHex());\n  console.log(roundTrip(\"héllo world\"), exponent(8));\n  try {\n    length(\"a\\0b\");\n  } catch (e) {\n    console.log(e.message);\n  }\n}\n",
    );
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "6 [ 42, 'abc' ] true true -31\nhéllo world|h|104,195 [ 0.5, 4 ]\na CString can't hold a NUL character: C would stop reading there\n",
            "velt {args:?}:\n{stderr}"
        );
    }
}

#[test]
fn raw_reads_need_unsafe_code() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { CString, Ptr, readCString } from \"velt/ffi\";\n\ndeclare c(\"c\") function getenv(name: CString): Ptr<u8> | null;\n\nexport function f(): string {\n  // SAFETY: getenv returns a string or NULL.\n  const p = unsafe { getenv(new CString(\"PATH\")) };\n  return p === null ? \"\" : readCString(p);\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  console.log(f());\n}\n",
    );
    let out = check(dir.path());
    assert!(
        out.contains("calling unsafe function `readCString`"),
        "{out}"
    );
}

/// A proxy over a `CString` could give C other bytes, without the NUL (its `get` trap answers
/// for the bytes): refused at the call, as for `Out`'s field.
#[test]
fn a_proxied_c_string_is_refused() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { CString } from \"velt/ffi\";\n\ndeclare c(\"c\") function strlen(s: CString): usize;\n\nexport function proxied(): usize {\n  const p: any = new Proxy(new CString(\"a\"), { get: (_t: any, _k: any) => [104, 105] });\n  const c = p as CString;\n  // SAFETY: a CString is NUL-terminated (a proxy is refused).\n  return unsafe { strlen(c) };\n}\n",
        "import { proxied } from \"cmath\";\n\nfunction main() {\n  console.log(proxied());\n}\n",
    );
    let o = velt(dir.path(), &["run"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{stderr}");
    assert!(
        stderr.contains("TypeError: this operation on a proxy"),
        "{stderr}"
    );
}

/// A pointer C can hold points at a C type, a number, `bool`, `void` or another pointer:
/// `Ptr<string>` and `OutPtr<string>` mean nothing in C.
#[test]
fn pointers_point_at_c_types_and_numbers() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { OutPtr, Ptr } from \"velt/ffi\";\n\ndeclare c(\"c\") function f(p: Ptr<string>): void;\ndeclare c(\"c\") function g(p: OutPtr<string>): void;\ndeclare c(\"c\") function h(p: Ptr<Ptr<u8>>, q: Ptr<void> | null): Ptr<f64>;\n\nexport const x = 1;\n",
        "import { x } from \"cmath\";\n\nfunction main() {\n  console.log(x);\n}\n",
    );
    let out = check(dir.path());
    assert!(
        out.contains("parameter `p` of C function `f` has type `Ptr<string>`"),
        "{out}"
    );
    assert!(
        out.contains("parameter `p` of C function `g` has type `OutPtr<string>`"),
        "{out}"
    );
    assert!(!out.contains("C function `h`"), "{out}");
}

/// `CString.bytesOf` lends the `CString`'s own bytes, read-only (zero-copy, as Rust's
/// `CString::as_bytes`): writing through them must be an error, or C would read past a removed
/// NUL. Two gaps in sema's `readonly` rule let them through today (#2228).
#[test]
#[ignore = "needs #2228: `readonly T[] as T[]` and locals inferred from a readonly array are writable"]
fn a_c_string_s_bytes_are_read_only() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "import { CString } from \"velt/ffi\";\n\nexport function f(): void {\n  const c = new CString(\"abc\");\n  const b = CString.bytesOf(c) as u8[];\n  b[3] = 65;\n  const r = CString.bytesOf(c);\n  r.push(66);\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  f();\n}\n",
    );
    let out = check(dir.path());
    assert!(out.matches("read-only").count() >= 2, "{out}");
}
