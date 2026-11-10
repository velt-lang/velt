//! `declare c` functions checked against their library's C headers (docs/internals/design/
//! c-ffi.md "Checking declarations against C headers", #2187 layer 6): the pinned zig's clang
//! parses them for the build's target. The tests that run clang need `VELT_ZIG` (a zig of the
//! pinned version, `velt_toolchain::zig::VERSION`), so they never download it; without it they
//! say so and pass. The comparison itself is unit-tested in `native::headers`.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;
mod zig_env;

/// A package `bind` (manifest fields `native` after its version; `lib`, its source; `files`,
/// more files) and a program `app` that depends on it and allows its native code.
fn project(dir: &Path, native: &str, lib: &str, files: &[(&str, &str)], allow: &str) {
    let pkg = dir.join("bind");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"bind\", version: \"0.1.0\"{native} }};\n"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    for (path, text) in files {
        let path = pkg.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(
        app.join("package.velt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  {allow}dependencies: {{ bind: {{ path: \"../bind\" }} }},\n}};\n"),
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.velt"),
        "import { len } from \"bind\";\n\nfunction main() {\n  console.log(len(\"abc\"));\n}\n",
    )
    .unwrap();
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

fn output(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const ALLOW: &str = "allowNative: [\"bind\"], ";

const LEN: &str = "export function len(s: string): usize {\n  // SAFETY: a CString is NUL-terminated.\n  return unsafe { strlen(new CString(s)) };\n}\n";

/// The zig to check with: [`zig_env::zig`].
fn zig() -> Option<PathBuf> {
    zig_env::zig("run clang")
}

const DECLS: &str = r#"import { CString, OutPtr, Ptr } from "velt/ffi";

declare c type thing;

declare c("c") function strlen(s: CString): usize;
declare c("c") function strtoll(s: CString, end: OutPtr<u8>, base: i32): i64;
declare c("c") function frexp(x: f64, exp: i32[]): f64;
declare c("c") function labs(x: i64): i64;
declare c("c") function malloc(n: usize): Ptr<u8> | null;
declare c("own") function thing_new(size: usize): Ptr<thing>;
declare c("own") function thing_free(t: Ptr<thing>): void;

"#;

const OWN_H: &str = "#include <stddef.h>\ntypedef struct thing thing;\nthing *thing_new(size_t size);\nvoid thing_free(thing *t);\nint thing_count(const thing *t);\n";

const NATIVE: &str = ", native: { c: { c: { headers: [\"<string.h>\", \"<stdlib.h>\", \"<math.h>\"] }, own: { system: { default: \"own\" }, headers: [\"native/own.h\"] } } }";

#[test]
fn declarations_are_checked_against_their_headers_per_target() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H)];
    project(dir.path(), NATIVE, &format!("{DECLS}{LEN}"), &files, ALLOW);
    // Right on this machine's 64-bit targets.
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-unknown-linux-gnu"],
    );
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers checked)"),
        "{}",
        output(&o)
    );
    // Win64's `long` is 32 bits: `labs` is `i32` there.
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-pc-windows-msvc"],
    );
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("error: parameter 1 of C function `labs` is `long` in stdlib.h:"),
        "{out}"
    );
    assert!(
        out.contains("not the declaration's `i64`\n  = note: declare `i32`"),
        "{out}"
    );
    // Wrong declarations, each named with the header's line.
    let wrong = DECLS
        .replace("declare c(\"c\") function labs", "declare c(\"c\") function strlen$wrong(s: CString): u64;\ndeclare c(\"c\") function labs")
        .replace("frexp(x: f64, exp: i32[])", "frexp(x: f64, exp: readonly i32[])")
        .replace("thing_free(t: Ptr<thing>)", "thing_free(t: Ptr<u8>)")
        .replace("declare c(\"own\") function thing_new", "declare c(\"own\") function thing_count(t: Ptr<thing>): i32;\ndeclare c(\"own\") function thing_gone(): void;\ndeclare c(\"own\") function thing_new");
    project(dir.path(), NATIVE, &format!("{wrong}{LEN}"), &files, ALLOW);
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-unknown-linux-gnu"],
    );
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for e in [
        "error: C function `strlen` returns `__size_t` in string.h:",
        "not the declaration's `u64`\n  = note: declare `usize`",
        "error: parameter 1 of C function `thing_free` is `thing *` in own.h:4, not the declaration's `Ptr<u8>`\n  = note: it points at C type `thing`: declare `Ptr<thing>`, with `declare c type thing;`",
        "error: C function `thing_gone` is not declared in its library's headers (native/own.h)",
    ] {
        assert!(out.contains(e), "{e}:\n{out}");
    }
    // A read-only array matches a non-const `int *` (many APIs leave `const` out), though
    // `frexp` writes it: the header can't tell.
    assert!(!out.contains("`frexp`"), "{out}");
    // `const thing *` takes a `Ptr<thing>`.
    assert!(!out.contains("`thing_count`"), "{out}");
    assert_eq!(out.matches("error:").count(), 3, "{out}");
}

/// One machine checks every target: `long` is 64 bits except on Windows.
#[test]
fn every_target_is_checked_with_its_own_types() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let decls = "import { CString } from \"velt/ffi\";\n\ndeclare c(\"c\") function strlen(s: CString): usize;\ndeclare c(\"c\") function labs(x: i64): i64;\n\n";
    let native = ", native: { c: { c: { headers: [\"<string.h>\", \"<stdlib.h>\"] } } }";
    project(dir.path(), native, &format!("{decls}{LEN}"), &[], ALLOW);
    for target in [
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        let o = velt(dir.path(), &home, &["check", "--target", target]);
        let out = output(&o);
        if target.contains("windows") {
            assert!(!o.status.success(), "{target}: {out}");
            assert!(
                out.contains("error: parameter 1 of C function `labs` is `long`"),
                "{target}: {out}"
            );
        } else {
            assert!(o.status.success(), "{target}: {out}");
        }
    }
}

#[test]
fn a_check_that_passed_is_remembered() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H)];
    project(dir.path(), NATIVE, &format!("{DECLS}{LEN}"), &files, ALLOW);
    let args = ["check", "--target", "x86_64-unknown-linux-gnu"];
    assert!(velt(dir.path(), &home, &args).status.success());
    // One entry per library: `c` and `own`.
    let remembered = std::fs::read_dir(home.join("cache/headers"))
        .unwrap()
        .count();
    assert_eq!(remembered, 2);
    // A changed header is checked again: here it changes `thing_free`'s parameter.
    let changed = OWN_H.replace("void thing_free(thing *t);", "void thing_free(int t);");
    std::fs::write(dir.path().join("bind/native/own.h"), changed).unwrap();
    let o = velt(dir.path(), &home, &args);
    assert!(!o.status.success());
    assert!(
        output(&o).contains("parameter 1 of C function `thing_free` is `int` in own.h:4"),
        "{}",
        output(&o)
    );
}

/// `velt check` in `app` for `target`, with a zig that doesn't exist: passing shows that clang
/// wasn't needed.
fn check_without_zig(dir: &Path, home: &Path, target: &str) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "--target", target])
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .env("VELT_ZIG", dir.join("no-zig"))
        .output()
        .unwrap()
}

/// `int (void) __attribute__((__cdecl__))`: what Windows headers declare most things as.
#[test]
fn attributed_function_types_are_read_on_windows_targets() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let decls = r#"import { CString, Ptr } from "velt/ffi";

declare c("c") function getpid(): i32;
declare c("c") function _getpid(): i32;
declare c("c") function _strdup(s: CString): Ptr<u8> | null;
declare c("c") function getenv(name: CString): Ptr<u8> | null;
declare c("c") function close(fd: i32): i32;
declare c("c") function _close(fd: i32): i32;
declare c("c") function write(fd: i32, data: readonly u8[], n: u32): i32;
declare c("c") function strlen(s: CString): usize;

"#;
    let native = ", native: { c: { c: { headers: [\"<process.h>\", \"<string.h>\", \"<stdlib.h>\", \"<io.h>\"] } } }";
    project(dir.path(), native, &format!("{decls}{LEN}"), &[], ALLOW);
    for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
        let o = velt(dir.path(), &home, &["check", "--target", target]);
        assert!(o.status.success(), "{target}: {}", output(&o));
    }
    // A mismatch is still one, and variadic behind an attribute is still variadic.
    let wrong = decls.replace(
        "getenv(name: CString): Ptr<u8> | null",
        "getenv(name: CString): i32",
    );
    let wrong = format!("{wrong}declare c(\"c\") function _cprintf(s: CString): i32;\n{LEN}");
    let native = native.replace("\"<io.h>\"", "\"<io.h>\", \"<conio.h>\"");
    project(dir.path(), &native, &wrong, &[], ALLOW);
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-pc-windows-msvc"],
    );
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("C function `getenv` returns `char *` in stdlib.h:"),
        "{out}"
    );
    assert!(
        out.contains("C function `_cprintf` is variadic in conio.h:"),
        "{out}"
    );
    assert_eq!(out.matches("error:").count(), 2, "{out}");
}

/// `GetLastError`, whose header takes most of `windows.h` to parse.
#[test]
fn a_win32_function_is_read_on_windows_targets() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let lib = format!("declare c(\"kernel32\") function GetLastError(): u32;\n{DECLS_STRLEN}{LEN}");
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, kernel32: { system: { win32: \"kernel32\" }, headers: [\"<windef.h>\", \"<errhandlingapi.h>\"] } } }";
    project(dir.path(), native, &lib, &[], ALLOW);
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-pc-windows-msvc"],
    );
    assert!(o.status.success(), "{}", output(&o));
}

const DECLS_STRLEN: &str = "import { CString } from \"velt/ffi\";\n\ndeclare c(\"c\") function strlen(s: CString): usize;\n\n";

/// A remembered check covers what a listed header includes, not only the header.
#[test]
fn a_change_in_an_included_header_is_checked_again() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [
        ("native/t.h", "#include \"inner.h\"\n"),
        ("native/inner.h", "int f(int a);\n"),
    ];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, z: { system: { default: \"z\" }, headers: [\"native/t.h\"] } } }";
    let lib = format!("declare c(\"z\") function f(a: i32): i32;\n{DECLS_STRLEN}{LEN}");
    project(dir.path(), native, &lib, &files, ALLOW);
    let target = "x86_64-unknown-linux-gnu";
    let args = ["check", "--target", target];
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("z (z; headers checked)"),
        "{}",
        output(&o)
    );
    // Remembered: clang doesn't run while nothing changed.
    let o = check_without_zig(dir.path(), &home, target);
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("z (z; headers checked)"),
        "{}",
        output(&o)
    );
    std::fs::write(dir.path().join("bind/native/inner.h"), "int f(long a);\n").unwrap();
    let o = velt(dir.path(), &home, &args);
    assert!(!o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("parameter 1 of C function `f` is `long` in inner.h:1"),
        "{}",
        output(&o)
    );
    // And back: the check passes again, not from what was remembered of the wrong one.
    std::fs::write(dir.path().join("bind/native/inner.h"), "int f(int a);\n").unwrap();
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
}

/// What an enum passes, from the width of its type.
#[test]
fn enums_are_as_wide_as_their_types() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "enum wide { A = 0x100000000LL };\nenum wide wide_f(void);\nenum big : long long { B };\nenum big big_f(void);\nenum byte : char { C };\nenum byte byte_f(enum byte x);\nenum small { D };\nenum small small_f(void);\ntypedef enum { E = 0x100000000LL } named;\nnamed named_f(void);\nenum __attribute__((packed)) tiny { F };\nenum tiny tiny_f(void);\nint proto_f(enum p : char { G } x);\nenum m { H } __attribute__((mode(byte)));\nenum m mode_f(void);\n";
    let files = [("native/e.h", header)];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, e: { system: { default: \"e\" }, headers: [\"native/e.h\"] } } }";
    let lib = |decls: &str| format!("{decls}{DECLS_STRLEN}{LEN}");
    let args = ["check", "--target", "x86_64-unknown-linux-gnu"];
    let right = "declare c(\"e\") function wide_f(): i64;\ndeclare c(\"e\") function big_f(): i64;\ndeclare c(\"e\") function byte_f(x: i8): u8;\ndeclare c(\"e\") function small_f(): i32;\ndeclare c(\"e\") function named_f(): i64;\n";
    project(dir.path(), native, &lib(right), &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    let wrong = "declare c(\"e\") function wide_f(): i32;\ndeclare c(\"e\") function big_f(): i32;\ndeclare c(\"e\") function byte_f(x: i32): i32;\ndeclare c(\"e\") function small_f(): i64;\ndeclare c(\"e\") function named_f(): i32;\ndeclare c(\"e\") function tiny_f(): i32;\ndeclare c(\"e\") function proto_f(x: i32): i32;\ndeclare c(\"e\") function mode_f(): i32;\n";
    project(dir.path(), native, &lib(wrong), &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for e in [
        "C function `wide_f` returns `enum wide` in e.h:2, not the declaration's `i32`\n  = note: declare `i64` (a C enum)",
        "C function `big_f` returns `enum big` in e.h:4, not the declaration's `i32`\n  = note: declare `i64` (a C enum)",
        "parameter 1 of C function `byte_f` is `enum byte` in e.h:6, not the declaration's `i32`\n  = note: declare `i8` (a C enum)",
        "C function `byte_f` returns `enum byte` in e.h:6, not the declaration's `i32`",
        "C function `small_f` returns `enum small` in e.h:8, not the declaration's `i64`\n  = note: declare `i32` (a C enum)",
        "C function `named_f` returns `named` in e.h:10, not the declaration's `i32`\n  = note: declare `i64` (a C enum)",
        "C function `tiny_f` returns `enum tiny` in e.h:12, not the declaration's `i32`\n  = note: declare `packed enum tiny`, which can't cross",
        // Defined in the parameters, where clang's dump doesn't show it: its width isn't known.
        "parameter 1 of C function `proto_f` is `enum p` in e.h:13, not the declaration's `i32`\n  = note: declare `enum p`, which can't cross",
        "C function `mode_f` returns `enum m` in e.h:15, not the declaration's `i32`\n  = note: declare `mode enum m`, which can't cross",
    ] {
        assert!(out.contains(e), "{e}:\n{out}");
    }
}

/// A declaration ending with `...args: CVarArgs` matches a variadic prototype, and only such a
/// one: each mismatch is an error with the fix. The fixed parameters compare as any function's.
#[test]
fn variadic_declarations_match_variadic_prototypes() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/v.h", "int vf(int a, ...);\nint fixed(int a);\n")];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, v: { system: { default: \"v\" }, headers: [\"native/v.h\"] } } }";
    let lib = |decls: &str| {
        format!("import {{ CVarArgs }} from \"velt/ffi\";\n{decls}{DECLS_STRLEN}{LEN}")
    };
    let args = ["check", "--target", "aarch64-apple-darwin"];
    let right = "declare c(\"v\") function vf(a: i32, ...args: CVarArgs): i32;\ndeclare c(\"v\") function fixed(a: i32): i32;\n";
    project(dir.path(), native, &lib(right), &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    let wrong = "declare c(\"v\") function vf(a: i32): i32;\ndeclare c(\"v\") function fixed(a: i64, ...args: CVarArgs): i32;\n";
    project(dir.path(), native, &lib(wrong), &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for e in [
        "C function `vf` is variadic in v.h:1\n  = note: declare its `...` last: `...args: CVarArgs` (velt/ffi)",
        "C function `fixed` is not variadic in v.h:2\n  = note: leave out `...args: CVarArgs`",
    ] {
        assert!(out.contains(e), "{e}:\n{out}");
    }
    assert_eq!(out.matches("error:").count(), 2, "{out}");
}

/// `declare c` calls with the target's C calling convention: a function with another is
/// refused, and Windows' own (`ms_abi`) or every other target's (`sysv_abi`) is that convention.
#[test]
fn a_calling_convention_other_than_the_target_s_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "int ms(int a) __attribute__((ms_abi));\nint sysv(int a) __attribute__((sysv_abi));\nint vector(int a) __attribute__((vectorcall));\nint plain(int a) __attribute__((cdecl));\n";
    let files = [("native/cc.h", header)];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, cc: { system: { default: \"cc\" }, headers: [\"native/cc.h\"] } } }";
    let decls = "declare c(\"cc\") function ms(a: i32): i32;\ndeclare c(\"cc\") function sysv(a: i32): i32;\ndeclare c(\"cc\") function vector(a: i32): i32;\ndeclare c(\"cc\") function plain(a: i32): i32;\n";
    project(
        dir.path(),
        native,
        &format!("{decls}{DECLS_STRLEN}{LEN}"),
        &files,
        ALLOW,
    );
    for (target, refused) in [
        ("x86_64-unknown-linux-gnu", "ms_abi"),
        ("x86_64-pc-windows-msvc", "sysv_abi"),
    ] {
        let o = velt(dir.path(), &home, &["check", "--target", target]);
        let out = output(&o);
        assert!(!o.status.success(), "{target}: {out}");
        let name = if refused == "ms_abi" { "ms" } else { "sysv" };
        let line = if refused == "ms_abi" { 1 } else { 2 };
        for (f, attribute, line) in [(name, refused, line), ("vector", "vectorcall", 3)] {
            let e = format!("C function `{f}` is `__attribute__(({attribute}))` in cc.h:{line}, a calling convention `declare c` doesn't call");
            assert!(out.contains(&e), "{target}: {e}:\n{out}");
        }
        assert_eq!(out.matches("error:").count(), 2, "{target}: {out}");
    }
}

/// A build with nothing changed is skipped, but a changed header of a package's own is a stamp
/// input: the next build checks the C functions again, with no source changed.
#[test]
fn a_changed_header_checks_the_next_build_again() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "unsigned long long strlen(const char *s);\n";
    let files = [("native/own.h", header)];
    let native = ", native: { c: { c: { headers: [\"native/own.h\"] } } }";
    let lib = "import { CString } from \"velt/ffi\";\n\ndeclare c(\"c\") function strlen(s: CString): u64;\n\nexport function len(s: string): u64 {\n  // SAFETY: a CString is NUL-terminated.\n  return unsafe { strlen(new CString(s)) };\n}\n";
    project(dir.path(), native, lib, &files, ALLOW);
    for _ in 0..2 {
        let o = velt(dir.path(), &home, &["run"]);
        assert!(o.status.success(), "{}", output(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "3\n");
        let o = velt(dir.path(), &home, &["build", "-o", "out"]);
        assert!(o.status.success(), "{}", output(&o));
    }
    let wrong = "int strlen(const char *s);\n";
    std::fs::write(dir.path().join("bind/native/own.h"), wrong).unwrap();
    for args in [&["run"][..], &["build", "-o", "out"][..]] {
        let o = velt(dir.path(), &home, args);
        assert!(!o.status.success(), "{args:?}: {}", output(&o));
        assert!(
            output(&o).contains("C function `strlen` returns `int` in own.h:1"),
            "{args:?}: {}",
            output(&o)
        );
    }
}

/// A registry package's archive has its record but not its headers: what the check didn't
/// read is no stamp input, so a second build of a program using it compiles nothing.
#[test]
fn an_unchanged_build_with_a_registry_package_is_not_compiled_again() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "#include <stddef.h>\nsize_t strlen(const char *s);\n";
    let files = [("native/t.h", header)];
    let native = ", native: { c: { c: { headers: [\"native/t.h\"] } } }";
    let lib = format!("import {{ CString }} from \"velt/ffi\";\n\ndeclare c(\"c\") function strlen(s: CString): usize;\n\n{LEN}");
    project(dir.path(), native, &lib, &files, ALLOW);
    let bind = dir.path().join("bind");
    for args in [&["native", "headers"][..], &["publish"][..]] {
        let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(args)
            .current_dir(&bind)
            .env("NO_COLOR", "1")
            .env("VELT_HOME", &home)
            .env("VELT_REGISTRY", home.join("registry"))
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", output(&o));
    }
    let manifest = dir.path().join("app/package.velt");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("{ path: \"../bind\" }", "\"0.1.0\""),
    )
    .unwrap();
    // Built outside `app`, whose listing the stamp records.
    std::fs::create_dir(dir.path().join("out")).unwrap();
    let stamp = dir.path().join("out/app.build-stamp");
    let mut seen = Vec::new();
    for _ in 0..2 {
        let o = velt(dir.path(), &home, &["build", "-o", "../out/app"]);
        assert!(o.status.success(), "{}", output(&o));
        let written = std::fs::metadata(&stamp).unwrap().modified().unwrap();
        seen.push((written, std::fs::read_to_string(&stamp).unwrap()));
    }
    assert!(!seen[0].1.contains("native/t.h"), "{}", seen[0].1);
    assert_eq!(seen[0], seen[1], "the second build wrote the stamp again");
}

/// `const char **` is a pointer C writes (sqlite's `pzTail`); `char *const *` isn't.
#[test]
fn an_out_pointer_is_the_pointer_c_can_write() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "typedef struct stmt stmt;\nint prep(stmt *db, const char *sql, int n, stmt **out, const char **tail);\nint fixed(char *const *tail);\n";
    let files = [("native/p.h", header)];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, p: { system: { default: \"p\" }, headers: [\"native/p.h\"] } } }";
    let lib = "import { OutPtr, Ptr } from \"velt/ffi\";\n\ndeclare c type stmt;\n\ndeclare c(\"p\") function prep(db: Ptr<stmt>, sql: Ptr<u8>, n: i32, out: OutPtr<stmt>, tail: OutPtr<u8>): i32;\n";
    let args = ["check", "--target", "x86_64-unknown-linux-gnu"];
    let text = format!("{lib}{DECLS_STRLEN}{LEN}");
    project(dir.path(), native, &text, &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    let wrong = "declare c(\"p\") function fixed(tail: OutPtr<u8>): i32;\n";
    let text = format!("{lib}{wrong}{DECLS_STRLEN}{LEN}");
    project(dir.path(), native, &text, &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("parameter 1 of C function `fixed` is `char *const *` in p.h:3"),
        "{out}"
    );
}

/// "headers checked" is for a library checked on this target; these need no zig, as nothing
/// runs clang.
#[test]
fn the_listing_says_checked_only_for_what_was() {
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H)];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, own: { system: { default: \"own\" }, headers: [\"native/own.h\"] } } }";
    project(
        dir.path(),
        native,
        &format!("{DECLS_STRLEN}{LEN}"),
        &files,
        ALLOW,
    );
    // WebAssembly has no C headers: `check` looks at nothing there.
    let o = check_without_zig(dir.path(), &home, "wasm32-wasip1");
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers listed)"),
        "{}",
        output(&o)
    );
    assert!(!output(&o).contains("headers checked"), "{}", output(&o));
    // Nothing is declared of `own`: nothing is checked of it.
    let native = ", native: { c: { c: { unchecked: true }, own: { system: { default: \"own\" }, headers: [\"native/own.h\"] } } }";
    project(
        dir.path(),
        native,
        &format!("{DECLS_STRLEN}{LEN}"),
        &files,
        ALLOW,
    );
    let o = check_without_zig(dir.path(), &home, "x86_64-unknown-linux-gnu");
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers listed)"),
        "{}",
        output(&o)
    );
    assert!(
        output(&o).contains("c (the platform's; unchecked)"),
        "{}",
        output(&o)
    );
}

fn git(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", output(&o));
}

/// `velt check` of `app`, which depends on `bind` by git (its lockfile removed first, so the
/// latest commit is fetched), with a `VELT_ZIG` that doesn't exist: a fetched package's
/// declarations are compared with its record, and clang never runs.
fn check_from_git(dir: &Path, home: &Path) -> Output {
    let _ = std::fs::remove_file(dir.join("app/velt.lock.json"));
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "--target", "x86_64-unknown-linux-gnu"])
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .env("VELT_ZIG", dir.join("no-zig"))
        .output()
        .unwrap()
}

/// `velt native headers` records what passed on every target in `c-headers.json`; a build
/// that fetches the package (here by git) compares its declarations with that record.
#[test]
fn a_fetched_package_s_declarations_are_compared_with_its_record() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H)];
    // `labs` is right on 64-bit Linux and macOS only (see above).
    let decls = DECLS.replace("declare c(\"c\") function labs(x: i64): i64;\n", "");
    project(dir.path(), NATIVE, &format!("{decls}{LEN}"), &files, ALLOW);
    let bind = dir.path().join("bind");
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["native", "headers"])
        .current_dir(&bind)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", &home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("2 C libraries against its headers on 8 targets"),
        "{}",
        output(&o)
    );
    let record = vpm::c_headers::CHeaders::read(&bind).unwrap().unwrap();
    assert_eq!(record.checked.len(), 8);
    let own = record.library("x86_64-windows-gnu", "own").unwrap();
    assert_eq!(own.declarations.len(), 2);
    assert_eq!(own.headers.keys().collect::<Vec<_>>(), ["native/own.h"]);
    git(&bind, &["init", "-q", "-b", "main"]);
    git(&bind, &["add", "-A"]);
    git(&bind, &["commit", "-q", "-m", "checked"]);
    depend_by_git(dir.path());
    let o = check_from_git(dir.path(), &home);
    assert!(o.status.success(), "{}", output(&o));
    // A declaration changed after the check.
    let lib = std::fs::read_to_string(bind.join("src/lib.velt")).unwrap();
    let changed = lib.replace("thing_free(t: Ptr<thing>)", "thing_free(t: Ptr<u8>)");
    std::fs::write(bind.join("src/lib.velt"), changed).unwrap();
    git(&bind, &["commit", "-q", "-am", "changed"]);
    let out = output(&check_from_git(dir.path(), &home));
    assert!(
        out.contains("C function `thing_free` in package `bind` is not declared as its author checked against C library `own`'s headers for x86_64-linux-gnu"),
        "{out}"
    );
    assert_eq!(out.matches("error:").count(), 1, "{out}");
    // No record.
    git(&bind, &["rm", "-q", "c-headers.json"]);
    git(&bind, &["commit", "-q", "-m", "unrecorded"]);
    let out = output(&check_from_git(dir.path(), &home));
    assert!(
        out.contains("package `bind` declares C functions, but was published without a record"),
        "{out}"
    );
}

/// Make `app`'s dependency on `bind` a git dependency, on `bind`'s branch `main`.
fn depend_by_git(dir: &Path) {
    let url = dir.join("bind").display().to_string().replace('\\', "/");
    let url = match url.starts_with('/') {
        true => format!("file://{url}"),
        false => format!("file:///{url}"),
    };
    let manifest = dir.join("app/package.velt");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let text = text.replace(
        "path: \"../bind\"",
        &format!("git: \"{url}\", branch: \"main\""),
    );
    let text = text.replace(
        ALLOW,
        &format!("allowNative: [{{ name: \"bind\", git: \"{url}\" }}], "),
    );
    std::fs::write(&manifest, text).unwrap();
}

/// `velt native headers` in `bind`, and its first commit.
fn record_and_commit(dir: &Path, home: &Path) {
    let bind = dir.join("bind");
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["native", "headers"])
        .current_dir(&bind)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", output(&o));
    git(&bind, &["init", "-q", "-b", "main"]);
    git(&bind, &["add", "-A"]);
    git(&bind, &["commit", "-q", "-m", "checked"]);
}

/// A program loads what it imports of a package: however it imports it, each declaration it
/// compiles was checked, whichever others it leaves out.
#[test]
fn a_consumer_of_some_modules_is_compared_with_the_record() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let module = |name: &str, c: &str, body: &str| {
        format!("import {{ CString }} from \"velt/ffi\";\n\ndeclare c(\"c\") function {c};\n\nexport function {name}(s: string): i64 {{\n  // SAFETY: a CString is NUL-terminated.\n  return unsafe {{ {body} }} as i64;\n}}\n")
    };
    let a = module("al", "strlen(s: CString): usize", "strlen(new CString(s))");
    let b = module(
        "bl",
        "strcmp(a: CString, b: CString): i32",
        "strcmp(new CString(s), new CString(s))",
    );
    let extra = module(
        "xl",
        "strspn(a: CString, b: CString): usize",
        "strspn(new CString(s), new CString(s))",
    );
    let files = [
        ("src/a.velt", a.as_str()),
        ("src/b.velt", b.as_str()),
        ("src/extra.velt", extra.as_str()),
    ];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] } } }";
    let lib = "export { al } from \"./a\";\nexport { bl } from \"./b\";\n";
    project(dir.path(), native, lib, &files, ALLOW);
    record_and_commit(dir.path(), &home);
    depend_by_git(dir.path());
    let main = dir.path().join("app/src/main.velt");
    let program = |imports: &str, uses: &str| {
        format!("{imports}\n\nfunction main() {{\n  console.log({uses});\n}}\n")
    };
    let ways = [
        ("import { al } from \"bind/a\";", "al(\"x\")"),
        (
            "import { bl } from \"bind/b\";\nimport { al } from \"bind/a\";",
            "al(\"x\") + bl(\"x\")",
        ),
        (
            "import { bl } from \"bind/b\";\nimport { al } from \"bind\";",
            "al(\"x\") + bl(\"x\")",
        ),
        ("import { al, bl } from \"bind\";", "al(\"x\") + bl(\"x\")"),
        // A module the package's root doesn't import.
        ("import { xl } from \"bind/extra\";", "xl(\"x\")"),
    ];
    for (imports, uses) in ways {
        std::fs::write(&main, program(imports, uses)).unwrap();
        let o = check_from_git(dir.path(), &home);
        assert!(o.status.success(), "{imports}: {}", output(&o));
    }
    // A declaration changed after the check is refused where it is loaded, not where it isn't.
    let changed = b.replace(
        "strcmp(a: CString, b: CString): i32",
        "strcmp(a: CString, b: CString): i64",
    );
    let bind = dir.path().join("bind");
    std::fs::write(bind.join("src/b.velt"), changed).unwrap();
    git(&bind, &["commit", "-q", "-am", "changed"]);
    std::fs::write(&main, program(ways[0].0, ways[0].1)).unwrap();
    let o = check_from_git(dir.path(), &home);
    assert!(o.status.success(), "{}", output(&o));
    std::fs::write(&main, program(ways[1].0, ways[1].1)).unwrap();
    let out = output(&check_from_git(dir.path(), &home));
    assert!(
        out.contains("C function `strcmp` in package `bind` is not declared as its author checked"),
        "{out}"
    );
    assert_eq!(out.matches("error:").count(), 1, "{out}");
}

/// A git dependency has its headers: the declarations being as checked isn't enough when a
/// header, or what it includes, isn't.
#[test]
fn a_changed_header_of_a_git_dependency_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [
        (
            "native/own.h",
            "#include \"inner.h\"\ndouble cbrt(double x);\n",
        ),
        ("native/inner.h", "int inner(int a);\n"),
    ];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, own: { system: { default: \"own\" }, headers: [\"native/own.h\"] } } }";
    let lib = format!("declare c(\"own\") function cbrt(x: f64): f64;\ndeclare c(\"own\") function inner(a: i32): i32;\n{DECLS_STRLEN}{LEN}");
    project(dir.path(), native, &lib, &files, ALLOW);
    record_and_commit(dir.path(), &home);
    depend_by_git(dir.path());
    let bind = dir.path().join("bind");
    let o = check_from_git(dir.path(), &home);
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers checked)"),
        "{}",
        output(&o)
    );
    // The listing is about this target: on one without C headers nothing was compared.
    let o = check_without_zig(dir.path(), &home, "wasm32-wasip1");
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers listed)"),
        "{}",
        output(&o)
    );
    assert!(!output(&o).contains("headers checked"), "{}", output(&o));
    // The listed header changes; then, that restored, one it includes; then it is gone. The
    // declarations and the record stay as they are.
    let cases = [
        ("native/own.h", Some("#include \"inner.h\"\nlong cbrt(long x);\n"), "`native/own.h` of C library `own` in package `bind` is not the one its author checked"),
        ("native/inner.h", Some("int inner(long a);\n"), "`native/inner.h` of C library `own` in package `bind` is not the one its author checked"),
        ("native/own.h", None, "`native/own.h` of C library `own` in package `bind` is gone"),
    ];
    for (path, text, refusal) in cases {
        let before = std::fs::read_to_string(bind.join(path)).unwrap();
        match text {
            Some(text) => std::fs::write(bind.join(path), text).unwrap(),
            None => std::fs::remove_file(bind.join(path)).unwrap(),
        }
        git(&bind, &["add", "-A"]);
        git(&bind, &["commit", "-q", "-m", "changed"]);
        let o = check_from_git(dir.path(), &home);
        assert!(!o.status.success(), "{path}: {}", output(&o));
        assert!(output(&o).contains(refusal), "{refusal}:\n{}", output(&o));
        assert!(!output(&o).contains("headers checked"), "{}", output(&o));
        std::fs::write(bind.join(path), before).unwrap();
        git(&bind, &["add", "-A"]);
        git(&bind, &["commit", "-q", "-m", "restored"]);
    }
    let o = check_from_git(dir.path(), &home);
    assert!(o.status.success(), "{}", output(&o));
}

/// With no C functions there is nothing to record, and no empty file saying so.
#[test]
fn a_package_without_c_functions_has_no_record() {
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let lib = "export function len(s: string): i32 {\n  return 1;\n}\n";
    project(dir.path(), "", lib, &[], "");
    let bind = dir.path().join("bind");
    std::fs::write(bind.join(vpm::c_headers::FILE), "{ \"checked\": {} }").unwrap();
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["native", "headers"])
        .current_dir(&bind)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", &home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("the package declares no C functions"),
        "{}",
        output(&o)
    );
    assert!(!bind.join(vpm::c_headers::FILE).exists());
}

/// std's own `declare c("c")` functions (velt/fs's file descriptors, std/fs/fd/posix.velt) are
/// checked against each target's C library headers by `velt native headers --std`, as each
/// target's build loads them; a declaration that differs from the headers fails it.
#[test]
fn std_s_c_declarations_match_every_target_s_headers() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let std = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    let run = || {
        no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["native", "headers", "--std"])
            .current_dir(dir.path())
            .env("NO_COLOR", "1")
            .env("VELT_HOME", &home)
            .env("VELT_STD", &std)
            .output()
            .unwrap()
    };
    let o = run();
    assert!(o.status.success(), "{}", output(&o));
    assert!(output(&o).contains("on 8 targets"), "{}", output(&o));
    // `pread`'s offset is an `off_t`, 64 bits: a declaration of it with an `i32` there (never
    // called, so it changes no Velt types) is refused on every POSIX target.
    let posix = std.join("fs/fd/posix.velt");
    let mut text = std::fs::read_to_string(&posix).unwrap();
    text.push_str("\ndeclare c(\"c\") function pread$wrong(fd: i32, buf: u8[], nbyte: usize, offset: i32): isize;\n");
    std::fs::write(&posix, text).unwrap();
    let o = run();
    assert!(!o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("parameter 4 of C function `pread`"),
        "{}",
        output(&o)
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}
