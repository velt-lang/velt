//! `declare c` functions checked against their library's C headers (docs/internals/design/
//! c-ffi.md "Checking declarations against C headers", #2187 layer 6): the pinned zig's clang
//! parses them for the build's target. The tests that run clang need `VELT_ZIG` (a zig of the
//! pinned version, `velt_toolchain::zig::VERSION`), so they never download it; without it they
//! say so and pass. The comparison itself is unit-tested in `native::headers`.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

/// A package `bind` (manifest fields `native` after its version; `lib`, its source; `files`,
/// more files) and a program `app` that depends on it and allows its native code.
fn project(dir: &Path, native: &str, lib: &str, files: &[(&str, &str)], allow: &str) {
    let pkg = dir.join("bind");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"bind\", version: \"0.1.0\"{native} }};\n"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.vlt"), lib).unwrap();
    for (path, text) in files {
        let path = pkg.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    std::fs::write(
        app.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  {allow}dependencies: {{ bind: {{ path: \"../bind\" }} }},\n}};\n"),
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.vlt"),
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

/// The zig to check with, or `None` (and a note) when `VELT_ZIG` names none.
fn zig() -> Option<PathBuf> {
    match std::env::var_os("VELT_ZIG") {
        Some(z) => Some(PathBuf::from(z)),
        None => {
            eprintln!("skipped: set VELT_ZIG to a zig 0.17.1 to run clang");
            None
        }
    }
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
    let header = "enum wide { A = 0x100000000LL };\nenum wide wide_f(void);\nenum big : long long { B };\nenum big big_f(void);\nenum byte : char { C };\nenum byte byte_f(enum byte x);\nenum small { D };\nenum small small_f(void);\ntypedef enum { E = 0x100000000LL } named;\nnamed named_f(void);\nenum __attribute__((packed)) tiny { F };\nenum tiny tiny_f(void);\n";
    let files = [("native/e.h", header)];
    let native = ", native: { c: { c: { headers: [\"<string.h>\"] }, e: { system: { default: \"e\" }, headers: [\"native/e.h\"] } } }";
    let lib = |decls: &str| format!("{decls}{DECLS_STRLEN}{LEN}");
    let args = ["check", "--target", "x86_64-unknown-linux-gnu"];
    let right = "declare c(\"e\") function wide_f(): i64;\ndeclare c(\"e\") function big_f(): i64;\ndeclare c(\"e\") function byte_f(x: i8): u8;\ndeclare c(\"e\") function small_f(): i32;\ndeclare c(\"e\") function named_f(): i64;\n";
    project(dir.path(), native, &lib(right), &files, ALLOW);
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    let wrong = "declare c(\"e\") function wide_f(): i32;\ndeclare c(\"e\") function big_f(): i32;\ndeclare c(\"e\") function byte_f(x: i32): i32;\ndeclare c(\"e\") function small_f(): i64;\ndeclare c(\"e\") function named_f(): i32;\ndeclare c(\"e\") function tiny_f(): i32;\n";
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
    ] {
        assert!(out.contains(e), "{e}:\n{out}");
    }
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

/// A fetched package's headers aren't compared at this layer, so they aren't called checked.
#[test]
fn a_fetched_package_s_headers_are_only_listed() {
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
    let bind = dir.path().join("bind");
    git(&bind, &["init", "-q", "-b", "main"]);
    git(&bind, &["add", "-A"]);
    git(&bind, &["commit", "-q", "-m", "first"]);
    let url = bind.display().to_string().replace('\\', "/");
    let url = match url.starts_with('/') {
        true => format!("file://{url}"),
        false => format!("file:///{url}"),
    };
    let manifest = dir.path().join("app/package.vlt");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let text = text.replace(
        "path: \"../bind\"",
        &format!("git: \"{url}\", branch: \"main\""),
    );
    std::fs::write(&manifest, text).unwrap();
    let o = check_without_zig(dir.path(), &home, "x86_64-unknown-linux-gnu");
    assert!(o.status.success(), "{}", output(&o));
    assert!(
        output(&o).contains("own (own; headers listed)"),
        "{}",
        output(&o)
    );
    assert!(
        output(&o).contains("c (the platform's; headers listed)"),
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
