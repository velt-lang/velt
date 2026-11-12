//! `declare c const` (docs/internals/design/c-ffi.md "Declarations"): a constant's value comes
//! from its package's C headers, per target. The tests that run clang need `VELT_ZIG`, as in
//! `c_headers.rs`; without it they say so and pass.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
// The `velt dev` session only: its HTTP and signal helpers serve tests/reload.rs.
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

const ALLOW: &str = "allowNative: [\"bind\"], ";

const IMPORT: &str = "import { Ptr } from \"velt/ffi\";\n\n";

/// A package `bind` (manifest fields `native` after its version; `lib`, its source; `files`,
/// more files) and a program `app` (its `main` the source) that depends on it.
fn project(dir: &Path, native: &str, lib: &str, files: &[(&str, &str)], main: &str, allow: &str) {
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
    std::fs::write(app.join("src/main.velt"), main).unwrap();
}

fn velt_in(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    velt_in(&dir.join("app"), home, args)
}

fn output(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The zig to read headers with: [`zig_env::zig`].
fn zig() -> Option<PathBuf> {
    zig_env::zig("run clang")
}

const OWN_H: &str = r#"#include "more/values.h"
typedef struct thing thing;
typedef void (*destructor)(void *);
#define OWN_STATIC ((destructor)0)
#define OWN_TRANSIENT ((destructor)-1)
#define OWN_THING ((thing *)16)
#define OWN_NULL_THING ((thing *)0)
#define OWN_SEVEN 7
#define OWN_NEG (-7)
#define OWN_BIG 0xFFFFFFFFFFFFFFFFull
#define OWN_FLAG 0x80000000u
#define OWN_E 2.5
#define OWN_F 1.5f
#define OWN_YES ((_Bool)1)
#define OWN_TEXT "text"
#define OWN_FUNCTION(x) (x)
enum own_kind { OWN_KIND_A = 3, OWN_KIND_B = -9 };
extern const int own_variable;
"#;

const VALUES_H: &str = "#define OWN_INCLUDED 41\n";

const NATIVE: &str =
    ", native: { c: { own: { system: { default: \"own\" }, headers: [\"native/own.h\"] } } }";

const LIB: &str = r#"import { Ptr } from "velt/ffi";

declare c type thing;

export declare c const OWN_SEVEN: i32;
export declare c const OWN_NEG: i64;
export declare c const OWN_BIG: u64;
export declare c const OWN_FLAG: u32;
export declare c const OWN_KIND_B: i8;
export declare c const OWN_E: f64;
export declare c const OWN_F: f32;
export declare c const OWN_YES: bool;
export declare c const OWN_INCLUDED: usize;
declare c const OWN_TRANSIENT: Ptr<void>;
declare c const OWN_STATIC: Ptr<void> | null;
declare c const OWN_THING: Ptr<thing>;
declare c const OWN_NULL_THING: Ptr<thing> | null;

const TABLE: i32[] = [OWN_SEVEN, 1];
const SUM: i32 = OWN_SEVEN + 1;

export function transient(): usize {
  return Ptr.addressOf(OWN_TRANSIENT);
}

export function hasStatic(): bool {
  return OWN_STATIC !== null;
}

export function thingAddress(): usize {
  return Ptr.addressOf(OWN_THING);
}

export function hasNullThing(): bool {
  return OWN_NULL_THING !== null;
}

export function table(): i32 {
  return TABLE[0] + SUM;
}
"#;

const MAIN: &str = r#"import {
  OWN_BIG,
  OWN_E,
  OWN_F,
  OWN_FLAG,
  OWN_INCLUDED,
  OWN_KIND_B,
  OWN_NEG,
  OWN_SEVEN,
  OWN_YES,
  hasNullThing,
  hasStatic,
  table,
  thingAddress,
  transient,
} from "bind";

function main() {
  console.log(`${OWN_SEVEN} ${OWN_NEG} ${OWN_BIG} ${OWN_FLAG} ${OWN_KIND_B}`);
  console.log(`${OWN_E} ${OWN_F} ${OWN_YES} ${OWN_INCLUDED}`);
  console.log(`${transient()} ${hasStatic()} ${thingAddress()} ${hasNullThing()} ${table()}`);
}
"#;

const PRINTED: &str = "7 -7 18446744073709551615 2147483648 -9\n2.5 1.5 true 41\n18446744073709551615 false 16 false 15\n";

/// Every kind of constant, read from the headers, in debug and release builds.
#[test]
fn a_constant_s_value_comes_from_the_headers() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H), ("native/more/values.h", VALUES_H)];
    project(dir.path(), NATIVE, LIB, &files, MAIN, ALLOW);
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(dir.path(), &home, args);
        assert!(o.status.success(), "velt {args:?}: {}", output(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), PRINTED, "velt {args:?}");
    }
    reload_support::build_runtime();
    std::fs::write(dir.path().join("app/main.velt"), MAIN).unwrap();
    let dev = reload_support::Dev::start(&dir.path().join("app"), &[]);
    dev.wait_stdout(
        reload_support::Mark::default(),
        "18446744073709551615 false 16 false 15",
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

/// Until `CFunction`, a function pointer crosses as `Ptr<void>`, which is how a constant like
/// `SQLITE_TRANSIENT` reaches the function that takes it; a typed pointer doesn't match.
#[test]
fn a_function_pointer_parameter_takes_a_void_pointer() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let header = "typedef void (*destructor)(void *);\n#define OWN_TRANSIENT ((destructor)-1)\nint own_bind(const char *text, destructor d);\n";
    let files = [("native/own.h", header)];
    let lib = |d: &str| {
        format!("{IMPORT}declare c const OWN_TRANSIENT: {d};\ndeclare c(\"own\") function own_bind(text: StrBytes, d: {d}): i32;\n\nexport function used(s: string): i32 {{\n  // SAFETY: SQLite-style: C copies the text because of the constant.\n  return unsafe {{ own_bind(s, OWN_TRANSIENT) }};\n}}\n")
            .replace("{ Ptr }", "{ Ptr, StrBytes }")
    };
    let main =
        "import { used } from \"bind\";\n\nfunction main() {\n  console.log(used(\"a\"));\n}\n";
    let args = ["check", "--target", "x86_64-unknown-linux-gnu"];
    project(dir.path(), NATIVE, &lib("Ptr<void>"), &files, main, ALLOW);
    let o = velt(dir.path(), &home, &args);
    assert!(o.status.success(), "{}", output(&o));
    project(dir.path(), NATIVE, &lib("Ptr<u8>"), &files, main, ALLOW);
    let out = output(&velt(dir.path(), &home, &args));
    assert!(
        out.contains("error: parameter 2 of C function `own_bind` is `destructor` in own.h:3, not the declaration's `Ptr<u8>`"),
        "{out}"
    );
}

/// A build with nothing changed isn't checked again, but a changed header, even one included by
/// another, changes the value. So does `velt check`'s remembered result.
#[test]
fn a_changed_header_changes_the_value() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H), ("native/more/values.h", VALUES_H)];
    let main = "import { OWN_INCLUDED } from \"bind\";\n\nfunction main() {\n  console.log(OWN_INCLUDED);\n}\n";
    project(dir.path(), NATIVE, LIB, &files, main, ALLOW);
    let run = |want: &str| {
        let o = velt(dir.path(), &home, &["run"]);
        assert!(o.status.success(), "{}", output(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), want);
    };
    run("41\n");
    run("41\n");
    std::fs::write(
        dir.path().join("bind/native/more/values.h"),
        "#define OWN_INCLUDED 42\n",
    )
    .unwrap();
    run("42\n");
}

/// What's declared has to be what the headers have, each named.
#[test]
fn a_wrong_constant_is_an_error() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let files = [("native/own.h", OWN_H), ("native/more/values.h", VALUES_H)];
    let lib = format!(
        "{IMPORT}declare c type thing;\ndeclare c type other;\n\ndeclare c const OWN_MISSING: i32;\ndeclare c const OWN_FUNCTION: i32;\ndeclare c const OWN_TEXT: Ptr<u8>;\ndeclare c const own_variable: i32;\ndeclare c const OWN_BIG: i64;\ndeclare c const OWN_FLAG: i32;\ndeclare c const OWN_E: f32;\ndeclare c const OWN_F: i32;\ndeclare c const OWN_SEVEN: f64;\ndeclare c const OWN_YES: i32;\ndeclare c const OWN_NEG: u32;\ndeclare c const OWN_THING: Ptr<other>;\ndeclare c const OWN_NULL_THING: Ptr<thing>;\ndeclare c const OWN_TRANSIENT: Ptr<thing>;\n\nexport function used(): i32 {{\n  return 1;\n}}\n"
    );
    let main = "import { used } from \"bind\";\n\nfunction main() {\n  console.log(used());\n}\n";
    project(dir.path(), NATIVE, &lib, &files, main, ALLOW);
    let o = velt(
        dir.path(),
        &home,
        &["check", "--target", "x86_64-unknown-linux-gnu"],
    );
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for e in [
        "error: C constant `OWN_MISSING` is not defined in its package's headers (native/own.h)",
        "error: C constant `OWN_FUNCTION` is not defined in its package's headers (native/own.h)",
        "error: C constant `OWN_TEXT` has no value a program can use: C gives it type `char[5]`",
        "error: C constant `own_variable` has no value a program can use: C gives it type `const int`",
        "error: C constant `OWN_BIG` is `18446744073709551615` of type `unsigned long long` in its headers, not the declaration's `i64`",
        "error: C constant `OWN_FLAG` is `2147483648` of type `unsigned int` in its headers, not the declaration's `i32`",
        "error: C constant `OWN_E` is `2.5` of type `double` in its headers, not the declaration's `f32`\n  = note: declare `f64`",
        "error: C constant `OWN_F` is `1.5` of type `float` in its headers, not the declaration's `i32`\n  = note: declare `f32`",
        "error: C constant `OWN_SEVEN` is `7` of type `int` in its headers, not the declaration's `f64`\n  = note: declare `i32`",
        "error: C constant `OWN_YES` is `true` of type `_Bool` in its headers, not the declaration's `i32`\n  = note: declare `bool`",
        "error: C constant `OWN_NEG` is `-7` of type `int` in its headers, not the declaration's `u32`",
        "error: C constant `OWN_THING` is `0x10` of type `thing *` in its headers, not the declaration's `Ptr<other>`",
        "error: C constant `OWN_NULL_THING` is `0x0` of type `thing *` in its headers, not the declaration's `Ptr<thing>`\n  = note: it is NULL: declare `Ptr<thing> | null`",
        "error: C constant `OWN_TRANSIENT` is `0xffffffffffffffff` of type `void (*)(void *)` in its headers, not the declaration's `Ptr<thing>`",
    ] {
        assert!(out.contains(e), "{e}:\n{out}");
    }
    assert_eq!(out.matches("error:").count(), 14, "{out}");
}

/// `velt native headers` records each target's value, here one that differs between them; a
/// fetched package's values come from that record, without clang.
#[test]
fn values_are_recorded_per_target_and_a_fetched_package_uses_them() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let lib = "export declare c const O_CREAT: i32;\n";
    let main =
        "import { O_CREAT } from \"bind\";\n\nfunction main() {\n  console.log(O_CREAT);\n}\n";
    let native = ", native: { c: { c: { headers: [\"<fcntl.h>\"] } } }";
    project(dir.path(), native, lib, &[], main, ALLOW);
    let bind = dir.path().join("bind");
    let o = velt_in(&bind, &home, &["native", "headers"]);
    assert!(o.status.success(), "{}", output(&o));
    let record = vpm::c_headers::CHeaders::read(&bind).unwrap().unwrap();
    assert_eq!(record.constants.len(), 8, "{record:?}");
    let value = |target: &str| match &record.constant(target, "O_CREAT").unwrap().value {
        vpm::c_headers::CConstantValue::Int(v) => v.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(value("x86_64-linux-gnu"), "64");
    assert_eq!(value("aarch64-macos"), "512");
    assert_eq!(value("x86_64-windows-gnu"), "256");
    // By git, as a package from the registry: `VELT_ZIG` names none, so clang can't run. Its
    // value on this machine is its record's, which here says something else, to see that.
    let mut changed = record.clone();
    let host = host_zig_target();
    changed
        .constants
        .get_mut(host)
        .unwrap()
        .values
        .get_mut("O_CREAT")
        .unwrap()
        .value = vpm::c_headers::CConstantValue::Int("1234".into());
    changed.write(&bind).unwrap();
    commit_and_depend_by_git(dir.path());
    let fetched = |args: &[&str]| fetched(dir.path(), &home, args);
    let o = fetched(&["run"]);
    assert!(o.status.success(), "{}", output(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "1234\n");
    // The declaration changed after the check.
    std::fs::write(
        bind.join("src/lib.velt"),
        "export declare c const O_CREAT: i64;\n",
    )
    .unwrap();
    git(&bind, &["commit", "-q", "-am", "changed"]);
    let out = output(&fetched(&["check"]));
    assert!(
        out.contains("error: C constant `O_CREAT` of package `bind` is not the one its author checked against its headers"),
        "{out}"
    );
    assert!(out.contains("c-headers.json records `i32`"), "{out}");
    // The declaration is as recorded, but the value isn't one it can hold: refused, where it
    // was compiled in and crashed lowering.
    std::fs::write(bind.join("src/lib.velt"), lib).unwrap();
    let mut wrong = changed.clone();
    wrong
        .constants
        .get_mut(host)
        .unwrap()
        .values
        .get_mut("O_CREAT")
        .unwrap()
        .value = vpm::c_headers::CConstantValue::Pointer("10".into());
    wrong.write(&bind).unwrap();
    git(&bind, &["commit", "-q", "-am", "a pointer"]);
    let o = fetched(&["run"]);
    let out = output(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("error: C constant `O_CREAT` of package `bind` is not the one its author checked against its headers"),
        "{out}"
    );
    // No value for a target.
    let mut gone = changed.clone();
    gone.constants.remove(host);
    gone.write(&bind).unwrap();
    git(&bind, &["commit", "-q", "-am", "no value"]);
    let out = output(&fetched(&["check"]));
    assert!(
        out.contains("error: package `bind` has no value of C constant `O_CREAT` for"),
        "{out}"
    );
    // No record.
    git(&bind, &["rm", "-q", "c-headers.json"]);
    git(&bind, &["commit", "-q", "-m", "unrecorded"]);
    let out = output(&fetched(&["check"]));
    assert!(
        out.contains("package `bind` declares C constants, but was published without a record of their values"),
        "{out}"
    );
}

fn host_zig_target() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-macos",
        ("x86_64", "macos") => "x86_64-macos",
        ("aarch64", "linux") => "aarch64-linux-gnu",
        ("x86_64", "linux") => "x86_64-linux-gnu",
        ("aarch64", "windows") => "aarch64-windows-gnu",
        _ => "x86_64-windows-gnu",
    }
}

/// Commit package `bind` in `dir` to a git repository, and make `app` depend on it by `file://`.
fn commit_and_depend_by_git(dir: &Path) {
    let bind = dir.join("bind");
    git(&bind, &["init", "-q", "-b", "main"]);
    git(&bind, &["add", "-A"]);
    git(&bind, &["commit", "-q", "-m", "checked"]);
    let url = bind.display().to_string().replace('\\', "/");
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

/// `velt <args>` in `app`, with `bind` fetched again from its branch, as a user without zig:
/// `VELT_ZIG` names none, so clang can't run.
fn fetched(dir: &Path, home: &Path, args: &[&str]) -> Output {
    let _ = std::fs::remove_file(dir.join("app/velt.lock.json"));
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .env("VELT_ZIG", dir.join("no-zig"))
        .output()
        .unwrap()
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

/// A value read through any file of the package changes with that file, wherever it is: what
/// clang read is what the remembered values and the build stamp hold.
#[test]
fn a_value_changes_with_any_header_it_was_read_through() {
    let Some(_zig) = zig() else { return };
    let lib = "export declare c const X: i32;\n";
    let main = "import { X } from \"bind\";\n\nfunction main() {\n  console.log(X);\n}\n";
    // The header listed, what it includes, and the file that defines `X`.
    let cases = [
        ("native/own.h", "#include \"../other/o.h\"\n", "other/o.h"),
        ("own.h", "#include \"inc/x.h\"\n", "inc/x.h"),
        ("native/own.h", "#include <shadow_x.h>\n", "shadow_x.h"),
    ];
    for (n, (listed, header, defines)) in cases.into_iter().enumerate() {
        let dir = test_dir::TestDir::new();
        let home = dir.path().join("home");
        let native = format!(
            ", native: {{ c: {{ own: {{ system: {{ default: \"own\" }}, headers: [\"{listed}\"] }} }} }}"
        );
        let value = 50 + 10 * n;
        let define = format!("#define X {value}\n");
        let files = [(listed, header), (defines, define.as_str())];
        project(dir.path(), &native, lib, &files, main, ALLOW);
        let run = |want: usize| {
            let o = velt(dir.path(), &home, &["run"]);
            assert!(o.status.success(), "{defines}: {}", output(&o));
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                format!("{want}\n"),
                "{defines}"
            );
        };
        run(value);
        let path = dir.path().join("bind").join(defines);
        std::fs::write(&path, format!("#define X {}\n", value + 1)).unwrap();
        run(value + 1);
    }
}

/// A git dependency with only constants has the headers its author read compared with the
/// record, as one with C functions has.
#[test]
fn a_changed_header_of_a_git_dependency_with_constants_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let lib = "export declare c const A: i32;\n";
    let main = "import { A } from \"bind\";\n\nfunction main() {\n  console.log(A);\n}\n";
    let files = [("native/own.h", "#define A 7\n")];
    project(dir.path(), NATIVE, lib, &files, main, ALLOW);
    let bind = dir.path().join("bind");
    let o = velt_in(&bind, &home, &["native", "headers"]);
    assert!(o.status.success(), "{}", output(&o));
    commit_and_depend_by_git(dir.path());
    let o = fetched(dir.path(), &home, &["run"]);
    assert!(o.status.success(), "{}", output(&o));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "7\n");
    std::fs::write(bind.join("native/own.h"), "#define A 8\n").unwrap();
    git(&bind, &["commit", "-q", "-am", "changed"]);
    let o = fetched(dir.path(), &home, &["run"]);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("error: header `native/own.h` of package `bind`'s C constants is not the one its author checked"),
        "{out}"
    );
}

const SEVEN: &str = "export declare c const SEVEN: i32;\n";
const USE_SEVEN: &str =
    "import { SEVEN } from \"bind\";\n\nfunction main() {\n  console.log(SEVEN);\n}\n";

/// Where a constant may be declared is what holds for a C function. None of this runs clang.
#[test]
fn a_constant_needs_a_native_package_the_program_allows() {
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    // No `native.c`.
    project(dir.path(), "", SEVEN, &[], USE_SEVEN, "");
    let out = output(&velt(dir.path(), &home, &["check"]));
    assert!(
        out.contains("error: package `bind` declares C constant `SEVEN`, but its package.velt doesn't say it has native code"),
        "{out}"
    );
    // Not allowed.
    let native = ", native: { c: { c: { headers: [\"native/own.h\"] } } }";
    project(dir.path(), native, SEVEN, &[], USE_SEVEN, "");
    let out = output(&velt(dir.path(), &home, &["check"]));
    assert!(out.contains("allowNative"), "{out}");
    // No headers to read.
    let unchecked = ", native: { c: { c: { unchecked: true } } }";
    project(dir.path(), unchecked, SEVEN, &[], USE_SEVEN, ALLOW);
    let out = output(&velt(dir.path(), &home, &["check"]));
    assert!(
        out.contains("error: C constant `SEVEN` takes its value from C headers, but package `bind` lists none"),
        "{out}"
    );
    // A file without a package.
    let lone = dir.path().join("lone.velt");
    std::fs::write(
        &lone,
        "declare c const SEVEN: i32;\n\nfunction main() {\n  console.log(SEVEN);\n}\n",
    )
    .unwrap();
    let o = velt_in(dir.path(), &home, &["check", "lone.velt"]);
    let out = output(&o);
    assert!(
        out.contains("error: `declare c const SEVEN` is only allowed in a package: this file has no package.velt"),
        "{out}"
    );
    // A type no constant has.
    let lib = "export declare c const SEVEN: string;\n";
    project(dir.path(), unchecked, lib, &[], USE_SEVEN, ALLOW);
    let out = output(&velt(dir.path(), &home, &["check"]));
    assert!(
        out.contains("error: C constant `SEVEN` has type `string`, which can't hold a C constant"),
        "{out}"
    );
}

/// WebAssembly has no C headers: reading a constant there is the error, as calling a C function
/// is, and no clang runs.
#[test]
fn webassembly_has_no_c_constants() {
    let dir = test_dir::TestDir::new();
    let home = dir.path().join("home");
    let native = ", native: { c: { c: { headers: [\"<fcntl.h>\"] } } }";
    project(dir.path(), native, SEVEN, &[], USE_SEVEN, ALLOW);
    let o = velt(dir.path(), &home, &["build", "--target", "wasm32-wasip1"]);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("error: C constant `SEVEN` is not available on wasm32-wasip1"),
        "{out}"
    );
    // Read through a table.
    let lib = "export declare c const SEVEN: i32;\nexport const TABLE: i32[] = [SEVEN, 2];\n";
    let main =
        "import { TABLE } from \"bind\";\n\nfunction main() {\n  console.log(TABLE[0]);\n}\n";
    project(dir.path(), native, lib, &[], main, ALLOW);
    let o = velt(dir.path(), &home, &["build", "--target", "wasm32-wasip1"]);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("error: C constant `SEVEN` is not available on wasm32-wasip1"),
        "{out}"
    );
}
