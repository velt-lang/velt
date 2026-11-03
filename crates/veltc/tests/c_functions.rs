//! C functions (`declare c("lib") function`, docs/internals/design/c-ffi.md, #2187 step 1): a
//! package declares them, calls them in `unsafe` code and gives programs a safe API; the calls
//! go straight to the C library with the C calling convention, in debug (Cranelift) and release
//! (LLVM) builds, with `errno` captured for `lastOsError()`. A program can't declare one.

use std::path::Path;
use std::process::Output;

mod no_window;
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

#[test]
fn one_symbol_has_one_signature() {
    let dir = test_dir::TestDir::new();
    project(
        dir.path(),
        "declare c(\"c\") function llabs$i(x: i64): i64;\ndeclare c(\"c\") function llabs$same(x: i64): i64;\ndeclare c(\"c\") function llabs$u(x: u32): u32;\n\nexport function f(): i64 {\n  // SAFETY: plain values.\n  return unsafe { llabs$i(-1) + llabs$same(-2) + (llabs$u(3) as i64) };\n}\n",
        "import { f } from \"cmath\";\n\nfunction main() {\n  console.log(f());\n}\n",
    );
    let o = velt(dir.path(), &["build"]);
    let out = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("C function `llabs` is called with two different signatures"),
        "{out}"
    );
    assert!(!out.contains("ICE"), "{out}");
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
        "declare c(\"c\") function puts(s: string): i32;\ndeclare c(\"c\") async function later(): void;\n\nexport const x = 1;\n",
        "import { x } from \"cmath\";\n\nfunction main() {\n  console.log(x);\n}\n",
    );
    let out = check(dir.path());
    for want in [
        "parameter `s` of C function `puts` has type `string`, which cannot cross to C",
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
