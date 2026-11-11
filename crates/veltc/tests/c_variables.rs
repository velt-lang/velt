//! C variables (`declare c("lib") let|const`, docs/internals/design/c-ffi.md "C variables") of
//! libraries every machine has, so these run in every test job without zig (tests/c_sources.rs
//! covers a package's own C sources): read and assigned in debug (Cranelift) and release (LLVM)
//! builds and under `velt dev` (the JIT), with velt/ffi's `setErrno` and `errno`; on Windows
//! through the import library's `__imp_<name>` when linked and by name under the JIT. A NULL in a
//! `Ptr<T>` variable stops the program, and `velt dev` names a variable no library has.

use std::path::Path;
use std::process::Output;

mod no_window;
#[allow(dead_code)]
// The `velt dev` session only: its HTTP and signal helpers serve tests/reload.rs.
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;

/// A package `cvars` (`lib`, calling the C libraries `native` maps, unchecked: the header check
/// needs zig) and a program `app` allowing its native code, whose `main` is `main` (also as
/// `app/main.velt`, the file `velt dev` runs).
fn project(dir: &Path, lib: &str, main: &str, native: &str) {
    let pkg = dir.join("cvars");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"cvars\", version: \"0.1.0\", native: {{ c: {{ {native} }} }} }};\n"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cvars\"],\n  dependencies: { cvars: { path: \"../cvars\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
    std::fs::write(app.join("main.velt"), main).unwrap();
}

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// libc's `opterr` (1 until a program sets it; glibc, musl and Apple's libc all define it): read,
/// `=`, `op=` and `++`; then `errno` set and read back.
#[cfg(unix)]
const LIB: &str = r#"import { errno, setErrno } from "velt/ffi";

declare c("c") let opterr: i32;

export function report(): string {
  let out = "";
  // SAFETY: `opterr` is libc's `int`, which only `getopt` (never called here) reads; `errno` is
  // this thread's.
  unsafe {
    const first = opterr;
    opterr = 5;
    opterr += 10;
    opterr++;
    const after = opterr;
    setErrno(34);
    out = `opterr ${first} ${after} errno ${errno()}`;
  }
  return out;
}
"#;

#[cfg(unix)]
const NATIVE: &str = "c: { unchecked: true }";

#[cfg(unix)]
const EXPECTED: &str = "opterr 1 16 errno 34";

/// ntdll's exported `NlsAnsiCodePage` (a DLL's variable, reached through `__imp_` when linked),
/// which is the code page kernel32's `GetACP()` returns: read, stored back unchanged and read
/// again; then `errno` set and read back.
#[cfg(windows)]
const LIB: &str = r#"import { errno, setErrno } from "velt/ffi";

declare c("ntdll") let NlsAnsiCodePage: u16;
declare c("kernel32") function GetACP(): u32;

export function report(): string {
  let out = "";
  // SAFETY: the code page is stored back as it was; `errno` is this thread's.
  unsafe {
    const page = NlsAnsiCodePage;
    NlsAnsiCodePage = page;
    const same = (NlsAnsiCodePage as u32) == GetACP() && page > 0;
    setErrno(34);
    out = `code page ${same} errno ${errno()}`;
  }
  return out;
}
"#;

#[cfg(windows)]
const NATIVE: &str = "ntdll: { system: { default: \"ntdll\" }, unchecked: true }, kernel32: { system: { default: \"kernel32\" }, unchecked: true }";

#[cfg(windows)]
const EXPECTED: &str = "code page true errno 34";

/// A library `NATIVE` maps, to declare a variable it doesn't have.
#[cfg(unix)]
const MISSING_FROM: &str = "c";
#[cfg(windows)]
const MISSING_FROM: &str = "ntdll";

const MAIN: &str =
    "import { report } from \"cvars\";\n\nfunction main() {\n  console.log(report());\n}\n";

#[test]
fn c_variables_are_read_and_assigned_in_debug_and_release_builds() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN, NATIVE);
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            format!("{EXPECTED}\n"),
            "velt {args:?}:\n{stderr}"
        );
    }
}

/// `velt dev` reads and assigns the variables as a linked build does (on Windows by the
/// variable's own name, which `GetProcAddress` resolves: no DLL exports `__imp_<name>`), and a C
/// variable no loaded library has is an error naming it, not a panic in the JIT.
#[test]
fn velt_dev_reaches_c_variables_and_reports_a_missing_one() {
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    project(dir.path(), LIB, MAIN, NATIVE);
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stdout(reload_support::Mark::default(), EXPECTED)
        .unwrap_or_else(|e| panic!("{e}"));
    drop(dev);
    let missing = LIB.replace(
        "export function report",
        &format!(
            "declare c(\"{MISSING_FROM}\") let no_such_var_r2722: i32;\n\nexport function report"
        ),
    );
    let missing = missing.replace(
        "  return out;",
        "  // SAFETY: never runs: the variable doesn't exist.\n  return out + unsafe { no_such_var_r2722 };",
    );
    std::fs::write(dir.path().join("cvars/src/lib.velt"), missing).unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stderr(
        reload_support::Mark::default(),
        "C variable `no_such_var_r2722` not found in the libraries loaded into `velt dev`",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let err = dev.stderr_since(reload_support::Mark::default()).join("\n");
    assert!(!err.contains("__imp_"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

/// A `Ptr<T>` variable holding NULL stops the program at the read (C has no non-null pointer
/// type, so the header check can't see it): libc's `optarg` is NULL until `getopt` sets it.
#[cfg(unix)]
#[test]
fn a_null_in_a_ptr_variable_stops_the_program() {
    let dir = test_dir::TestDir::new();
    let lib = "import { Ptr } from \"velt/ffi\";\n\ndeclare c(\"c\") let optarg: Ptr<u8>;\n\nexport function report(): string {\n  // SAFETY: `optarg` is libc's `char *`, read only.\n  const p = unsafe { optarg };\n  return `read ${Ptr.addressOf(p)}`;\n}\n";
    project(dir.path(), lib, MAIN, NATIVE);
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), args);
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success(), "velt {args:?} succeeded:\n{stderr}");
        assert!(
            stderr.contains("panic: C variable `optarg` is NULL, but its declaration's type is `Ptr<T>`: declare it `Ptr<T> | null`"),
            "velt {args:?}:\n{stderr}"
        );
        assert!(
            !String::from_utf8_lossy(&o.stdout).contains("read"),
            "velt {args:?}"
        );
    }
}
