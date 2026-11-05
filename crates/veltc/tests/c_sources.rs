//! A package that ships a C library as `sources` (docs/internals/design/c-ffi.md "Where libraries
//! come from"): `velt native build` compiles it with the pinned zig, reproducibly and for other
//! targets, debug builds link the shared library, release builds the static one, `velt dev` loads
//! it, and those who install the published package need no zig. These tests need `VELT_ZIG` (a zig
//! of the pinned version, `velt_toolchain::zig::VERSION`), so they never download it; without it
//! they say so and pass. What needs no zig is unit-tested in `vpm::native` and
//! `vpm::manifest::read`.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
mod reload_support;
mod runtime_support;
mod test_dir;

const HEADER: &str = "#include <stddef.h>\n\nint tiny_answer(void);\nsize_t tiny_len(const char *s);\ndouble tiny_root(double x);\nconst char *tiny_built(void);\n";

/// `ANSWER` comes from the manifest's `defines`, and the rest from the C library and libm.
/// `tiny_built` holds the source's path and the clock, which a build must not let in.
fn source(answer_offset: i32) -> String {
    format!(
        "#include \"tiny.h\"\n#include \"tiny_more.inc\"\n#include <math.h>\n#include <string.h>\n\nint tiny_answer(void) {{ return ANSWER + MORE + {answer_offset}; }}\nsize_t tiny_len(const char *s) {{ return strlen(s); }}\ndouble tiny_root(double x) {{ return sqrt(x); }}\nconst char *tiny_built(void) {{ return __FILE__ \" \" __DATE__ \" \" __TIME__; }}\n"
    )
}

const LIB: &str = "import { CString } from \"velt/ffi\";\n\ndeclare c(\"tiny\") function tiny_answer(): i32;\ndeclare c(\"tiny\") function tiny_len(s: CString): usize;\ndeclare c(\"tiny\") function tiny_root(x: f64): f64;\n\nexport function answer(): i32 {\n  // SAFETY: it takes nothing and reads nothing.\n  return unsafe { tiny_answer() };\n}\n\nexport function len(s: string): usize {\n  // SAFETY: a CString is NUL-terminated.\n  return unsafe { tiny_len(new CString(s)) };\n}\n\nexport function root(x: f64): f64 {\n  // SAFETY: it reads only its argument.\n  return unsafe { tiny_root(x) };\n}\n";

const MAIN: &str = "import { answer, len, root } from \"tiny\";\n\nfunction main() {\n  console.log(answer());\n  console.log(len(\"four\"));\n  console.log(root(81));\n}\n";

const TARGETS: &str = "targets: [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\", \"x86_64-apple-darwin\", \"aarch64-apple-darwin\", \"x86_64-pc-windows-msvc\", \"aarch64-pc-windows-msvc\"]";

fn native(extra: &str) -> String {
    format!(", native: {{ {extra}c: {{ tiny: {{ sources: [\"native/tiny.c\"], headers: [\"native/tiny.h\"], defines: {{ ANSWER: \"40\" }} }} }} }}")
}

/// The package `tiny` at `dir` (fields `manifest` after its version).
fn package(dir: &Path, manifest: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("native")).unwrap();
    std::fs::write(
        dir.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"tiny\", version: \"0.1.0\"{manifest} }};\n"),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.vlt"), LIB).unwrap();
    std::fs::write(dir.join("native/tiny.h"), HEADER).unwrap();
    std::fs::write(dir.join("native/tiny.c"), source(0)).unwrap();
    std::fs::write(dir.join("native/tiny_more.inc"), "#define MORE 2\n").unwrap();
}

/// A program depending on `dependency` (a manifest's entry), with `allow` among its fields.
fn app(dir: &Path, allow: &str, dependency: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  {allow}dependencies: {{ tiny: {dependency} }},\n}};\n"),
    )
    .unwrap();
    std::fs::write(dir.join("src/main.vlt"), MAIN).unwrap();
}

/// `dir/tiny` and `dir/app`, which allows it by path.
fn project(dir: &Path) -> PathBuf {
    package(&dir.join("tiny"), &native(""));
    let app_dir = dir.join("app");
    app(
        &app_dir,
        "allowNative: [\"tiny\"], ",
        "{ path: \"../tiny\" }",
    );
    app_dir
}

fn velt(dir: &Path, home: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = no_window::command(env!("CARGO_BIN_EXE_velt"));
    c.args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_REGISTRY_TOKEN");
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[track_caller]
fn ok(o: Output) -> Output {
    assert!(o.status.success(), "{}", text(&o));
    o
}

/// The zig to build with, or `None` (and a note) when `VELT_ZIG` names none.
fn zig() -> Option<PathBuf> {
    match std::env::var_os("VELT_ZIG") {
        Some(z) => Some(PathBuf::from(z)),
        None => {
            eprintln!("skipped: set VELT_ZIG to a zig 0.17.1 to build C sources");
            None
        }
    }
}

/// A shared cache for zig across tests: each target's C library stubs take it a while once.
fn home_with_zig_cache(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    home
}

fn run_host(app: &Path, home: &Path, envs: &[(&str, &str)], release: bool) -> String {
    let mut args = vec!["run"];
    if release {
        args.push("--release");
    }
    let o = ok(velt(app, home, envs, &args));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Debug links the shared library, release the static one, and `velt dev` loads it.
#[test]
fn a_package_s_c_sources_build_and_link_in_every_mode() {
    let Some(_zig) = zig() else { return };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    // A check needs no library built, and lists where this one comes from.
    let o = ok(velt(&app, &home, &[], &["check"]));
    assert!(
        text(&o).contains("tiny (built from native/tiny.c; headers checked)"),
        "{}",
        text(&o)
    );
    let bundles = dir.path().join("tiny/target/velt-native");
    assert!(!bundles.exists(), "`velt check` built the library");

    let expected = "42\n4\n9\n";
    assert_eq!(run_host(&app, &home, &[], false), expected);
    assert_eq!(run_host(&app, &home, &[], true), expected);
    let host = host_triple(&bundles);
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(bundles.join(&host).join("native.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["package"], "tiny");
    assert!(meta["c"]["tiny"]["shared"].is_string(), "{meta}");

    // The release executable holds the library: it runs with the bundle gone.
    let exe = dir.path().join(if cfg!(windows) {
        "tiny-app.exe"
    } else {
        "tiny-app"
    });
    let out = exe.to_str().unwrap();
    ok(velt(&app, &home, &[], &["build", "--release", "-o", out]));
    std::fs::rename(&bundles, bundles.with_extension("gone")).unwrap();
    let o = no_window::command(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), expected, "{}", text(&o));
    std::fs::rename(bundles.with_extension("gone"), &bundles).unwrap();

    // `velt dev` finds the library's functions in what it loaded.
    std::fs::write(app.join("main.vlt"), MAIN).unwrap();
    let env = [("VELT_HOME", home.to_str().unwrap())];
    let dev = reload_support::Dev::start_with_env(&app, &[], &env);
    dev.wait_stdout(reload_support::Mark::default(), "9")
        .unwrap_or_else(|e| panic!("{e}"));
}

/// The one directory under `bundles`, a build's target.
fn host_triple(bundles: &Path) -> String {
    let entries = std::fs::read_dir(bundles).unwrap();
    let dirs: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    dirs.into_iter().next().unwrap()
}

/// What the build's key covers: a changed source, a header the manifest doesn't name, and the
/// manifest's definitions each reach the program in debug and release builds, whether or not the
/// program's own files changed. An unchanged build runs no zig, and still links the library.
#[test]
fn a_changed_source_rebuilds_and_an_unchanged_one_does_not() {
    let Some(_zig) = zig() else { return };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    let tiny = dir.path().join("tiny");
    for release in [false, true] {
        assert_eq!(run_host(&app, &home, &[], release), "42\n4\n9\n");
    }

    // No zig to run: everything below would fail if a build looked for one.
    let no_zig = [("VELT_ZIG", "/velt-test/no-such-zig")];
    for release in [false, true] {
        assert_eq!(
            run_host(&app, &home, &no_zig, release),
            "42\n4\n9\n",
            "an unchanged build ran zig or lost its library (release: {release})"
        );
    }
    // A bundle gone is built again, though nothing else changed.
    let bundles = tiny.join("target/velt-native");
    std::fs::remove_dir_all(bundles.join(host_triple(&bundles))).unwrap();
    let o = velt(&app, &home, &no_zig, &["run", "--release"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("no-such-zig"), "{}", text(&o));
    assert_eq!(run_host(&app, &home, &[], true), "42\n4\n9\n");

    std::fs::write(tiny.join("native/tiny.c"), source(100)).unwrap();
    for release in [false, true] {
        assert_eq!(run_host(&app, &home, &[], release), "142\n4\n9\n");
    }
    std::fs::write(tiny.join("native/tiny_more.inc"), "#define MORE 3\n").unwrap();
    for release in [false, true] {
        assert_eq!(run_host(&app, &home, &[], release), "143\n4\n9\n");
    }
    let manifest = std::fs::read_to_string(tiny.join("package.vlt")).unwrap();
    std::fs::write(
        tiny.join("package.vlt"),
        manifest.replace("ANSWER: \"40\"", "ANSWER: \"50\""),
    )
    .unwrap();
    for release in [false, true] {
        assert_eq!(run_host(&app, &home, &[], release), "153\n4\n9\n");
    }
}

/// A source that doesn't compile is an error naming the file, and leaves the old bundle.
#[test]
fn a_source_that_does_not_compile_says_so() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    let tiny = dir.path().join("tiny");
    std::fs::write(
        tiny.join("native/tiny.c"),
        "int tiny_answer(void) { nope }\n",
    )
    .unwrap();
    let o = velt(&app, &home, &[], &["run"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("package `tiny`, C library `tiny`"), "{out}");
    assert!(out.contains("compiling `native/tiny.c`"), "{out}");
    assert!(out.contains("nope"), "{out}");
    std::fs::remove_file(tiny.join("native/tiny.c")).unwrap();
    let out = text(&velt(&app, &home, &[], &["run"]));
    assert!(
        out.contains("source `native/tiny.c` is not in the package"),
        "{out}"
    );
}

/// The header check reads a package's own `headers`, with the library's `defines`.
#[test]
fn declarations_are_checked_against_the_package_s_headers() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    let tiny = dir.path().join("tiny");
    let wrong = LIB
        .replace("tiny_answer(): i32", "tiny_answer(): i64")
        .replace("answer(): i32", "answer(): i64");
    std::fs::write(tiny.join("src/lib.vlt"), wrong).unwrap();
    let o = velt(&app, &home, &[], &["check"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("C function `tiny_answer` returns `int`"),
        "{out}"
    );

    // The same declaration is right where the library's `defines` make the header say so.
    std::fs::write(
        tiny.join("native/tiny.h"),
        format!(
            "#ifdef WIDE\nlong long tiny_answer(void);\n#else\nint tiny_answer(void);\n#endif\n{}",
            HEADER.replace("int tiny_answer(void);\n", "")
        ),
    )
    .unwrap();
    let manifest = std::fs::read_to_string(tiny.join("package.vlt")).unwrap();
    std::fs::write(
        tiny.join("package.vlt"),
        manifest.replace("ANSWER: \"40\"", "ANSWER: \"40\", WIDE: \"1\""),
    )
    .unwrap();
    ok(velt(&app, &home, &[], &["check"]));
}

/// `defines` change what a header declares, so the record holds them: a git dependency whose
/// `defines` changed after its author's check is refused, not run with a wrong signature (here a
/// `long long` result read as `i32`).
#[test]
fn changed_defines_are_refused_from_a_git_dependency() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let tiny = dir.path().join("tiny");
    package(&tiny, &native(""));
    let header = HEADER.replace(
        "int tiny_answer(void);\n",
        "#ifdef WIDE\nlong long tiny_answer(void);\n#else\nint tiny_answer(void);\n#endif\n",
    );
    std::fs::write(tiny.join("native/tiny.h"), header).unwrap();
    let wide = source(0).replace(
        "int tiny_answer(void) { return ANSWER + MORE + 0; }",
        "#ifdef WIDE\nlong long tiny_answer(void) { return 0x100000000LL + ANSWER - 2 + MORE; }\n#else\nint tiny_answer(void) { return ANSWER - 2 + MORE; }\n#endif",
    );
    assert_ne!(wide, source(0));
    std::fs::write(tiny.join("native/tiny.c"), wide).unwrap();
    ok(velt(&tiny, &home, &[], &["native", "headers"]));
    git(&tiny, &["init", "-q", "-b", "main"]);
    git(&tiny, &["add", "-A"]);
    git(&tiny, &["commit", "-q", "-m", "checked"]);
    let url = tiny.display().to_string().replace('\\', "/");
    let url = match url.starts_with('/') {
        true => format!("file://{url}"),
        false => format!("file:///{url}"),
    };
    let app_dir = dir.path().join("app");
    let dependency = format!("{{ git: \"{url}\", branch: \"main\" }}");
    let allow = format!("allowNative: [{{ name: \"tiny\", git: \"{url}\" }}], ");
    app(&app_dir, &allow, &dependency);
    let from_source = [("VELT_NATIVE_FROM_SOURCE", "1")];
    let o = ok(velt(&app_dir, &home, &from_source, &["check"]));
    assert!(text(&o).contains("headers checked"), "{}", text(&o));
    assert_eq!(run_host(&app_dir, &home, &from_source, false), "40\n4\n9\n");
    // A later commit adds `WIDE` and keeps the record.
    let manifest = std::fs::read_to_string(tiny.join("package.vlt")).unwrap();
    let widened = manifest.replace("ANSWER: \"40\"", "ANSWER: \"40\", WIDE: \"1\"");
    assert_ne!(manifest, widened);
    std::fs::write(tiny.join("package.vlt"), widened).unwrap();
    git(&tiny, &["commit", "-q", "-am", "wide"]);
    let _ = std::fs::remove_file(app_dir.join("velt.lock.json"));
    for args in [&["check"][..], &["run"][..]] {
        let o = velt(&app_dir, &home, &from_source, args);
        let out = text(&o);
        assert!(!o.status.success(), "{args:?}: {out}");
        assert!(
            out.contains("C library `tiny` in package `tiny` has other `defines` than its author checked its headers with"),
            "{args:?}: {out}"
        );
    }
}

/// A `declare c const` reads the library's headers with its `defines`, as its sources see them,
/// and a changed define reads the value again though no header changed.
#[test]
fn a_define_changes_a_constant_s_value() {
    let Some(_zig) = zig() else { return };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    let tiny = dir.path().join("tiny");
    std::fs::write(
        tiny.join("native/limit.h"),
        "#ifndef ANSWER\n#define ANSWER 1\n#endif\n#define TINY_LIMIT (ANSWER + 1)\n",
    )
    .unwrap();
    let manifest = std::fs::read_to_string(tiny.join("package.vlt")).unwrap();
    let limited = manifest.replace(
        "headers: [\"native/tiny.h\"]",
        "headers: [\"native/tiny.h\", \"native/limit.h\"]",
    );
    assert_ne!(manifest, limited);
    std::fs::write(tiny.join("package.vlt"), &limited).unwrap();
    let lib = format!("{LIB}\ndeclare c const TINY_LIMIT: i32;\n\nexport function limit(): i32 {{\n  return TINY_LIMIT;\n}}\n");
    std::fs::write(tiny.join("src/lib.vlt"), lib).unwrap();
    std::fs::write(
        app.join("src/main.vlt"),
        "import { answer, limit } from \"tiny\";\n\nfunction main() {\n  console.log(answer());\n  console.log(limit());\n}\n",
    )
    .unwrap();
    assert_eq!(run_host(&app, &home, &[], false), "42\n41\n");
    std::fs::write(
        tiny.join("package.vlt"),
        limited.replace("ANSWER: \"40\"", "ANSWER: \"50\""),
    )
    .unwrap();
    assert_eq!(run_host(&app, &home, &[], false), "52\n51\n");
}

/// A source may include only what is under `native/`, which the build is keyed on and the
/// package ships: a header elsewhere in the package (through `-I.`) is refused.
#[test]
fn an_include_outside_native_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let app = project(dir.path());
    let tiny = dir.path().join("tiny");
    std::fs::create_dir_all(tiny.join("inc")).unwrap();
    std::fs::write(tiny.join("inc/more.h"), "#define EXTRA 1\n").unwrap();
    let source = format!("#include \"inc/more.h\"\n{}", source(0));
    std::fs::write(tiny.join("native/tiny.c"), source).unwrap();
    let o = velt(&app, &home, &[], &["run"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("`native/tiny.c` includes `inc/more.h`, which is outside `native/`"),
        "{out}"
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
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
}

/// The bytes of every file of a bundle, by path.
fn bundle_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for sub in ["", "shared", "static"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().is_file() {
                let name = format!("{sub}/{}", e.file_name().to_string_lossy());
                out.push((name, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The format of an object or library file, by its first bytes.
fn format_of(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x7f, b'E', b'L', b'F', ..] => "elf",
        [0xcf, 0xfa, 0xed, 0xfe, ..] => "macho",
        [b'M', b'Z', ..] => "pe",
        [b'!', b'<', b'a', b'r', b'c', b'h', b'>', ..] => "archive",
        _ => "other",
    }
}

/// Every target builds from this machine, and two copies of a package, in other directories and at
/// other times, give the same bytes.
#[test]
fn every_target_cross_builds_reproducibly() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let targets = [
        ("x86_64-unknown-linux-gnu", "elf"),
        ("aarch64-unknown-linux-gnu", "elf"),
        ("x86_64-apple-darwin", "macho"),
        ("aarch64-apple-darwin", "macho"),
        ("x86_64-pc-windows-msvc", "pe"),
        ("aarch64-pc-windows-msvc", "pe"),
    ];
    let first = dir.path().join("first/tiny");
    let second = dir.path().join("elsewhere/deeper/tiny");
    package(&first, &native(""));
    package(&second, &native(""));
    for (target, format) in targets {
        let built: Vec<Vec<(String, Vec<u8>)>> = [&first, &second]
            .iter()
            .map(|p| {
                ok(velt(
                    p,
                    &home,
                    &[],
                    &["native", "build", "--target", target],
                ));
                bundle_files(&p.join("target/velt-native").join(target))
            })
            .collect();
        assert_eq!(
            built[0].iter().map(|(n, _)| n).collect::<Vec<_>>(),
            built[1].iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "{target}"
        );
        for ((name, a), (_, b)) in built[0].iter().zip(&built[1]) {
            assert!(a == b, "{target}: `{name}` differs between two builds");
        }
        let shared = built[0]
            .iter()
            .find(|(n, _)| n.starts_with("shared/") && !n.ends_with(".dll.lib"))
            .unwrap_or_else(|| panic!("{target} has no shared library"));
        assert_eq!(format_of(&shared.1), format, "{target}: {}", shared.0);
        let has = |prefix: &str, ext: &str| {
            built[0]
                .iter()
                .any(|(n, _)| n.starts_with(prefix) && n.ends_with(ext))
        };
        if target.contains("windows") {
            assert!(has("shared/", ".dll.lib"), "{target}: no import library");
            assert!(!has("static/", ""), "{target}");
        } else {
            assert!(has("static/", ".a"), "{target}: no static library");
        }
    }
}

/// The package published with its bundles to a registry, then added by a program on a machine with
/// no zig: `allowNative` is asked for, the prebuilt library is fetched and verified, and it builds
/// and runs in debug and release. A target nobody published says what to do.
#[test]
fn a_published_package_needs_no_zig() {
    let Some(_zig) = zig() else { return };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home_with_zig_cache(dir.path());
    let tiny = dir.path().join("tiny");
    package(&tiny, &native(&format!("{TARGETS}, ")));
    let o = ok(velt(&tiny, &home, &[], &["publish"]));
    assert!(
        text(&o).contains("prebuilt library for aarch64-apple-darwin"),
        "{}",
        text(&o)
    );
    // The record holds the digest of the bundled library's header, which the archive ships.
    let record = std::fs::read_to_string(tiny.join("c-headers.json")).unwrap();
    assert!(record.contains("\"native/tiny.h\""), "{record}");
    let index = std::fs::read_to_string(home.join("registry/tiny/index.json")).unwrap();
    for target in ["x86_64-pc-windows-msvc", "aarch64-unknown-linux-gnu"] {
        assert!(index.contains(target), "{index}");
    }

    let no_zig = [("VELT_ZIG", "/velt-test/no-such-zig")];
    let refused = dir.path().join("refused");
    app(&refused, "", "\"0.1.0\"");
    let out = text(&velt(&refused, &home, &no_zig, &["run"]));
    assert!(
        out.contains("package `tiny`") && out.contains("allowNative: [\"tiny\"]"),
        "{out}"
    );

    let user = dir.path().join("user");
    app(&user, "allowNative: [\"tiny\"], ", "\"0.1.0\"");
    assert_eq!(run_host(&user, &home, &no_zig, false), "42\n4\n9\n");
    assert_eq!(run_host(&user, &home, &no_zig, true), "42\n4\n9\n");
    let o = ok(velt(&user, &home, &no_zig, &["install"]));
    assert!(
        text(&o).contains("prebuilt, checksum verified"),
        "{}",
        text(&o)
    );
    let lock = std::fs::read_to_string(user.join("velt.lock.json")).unwrap();
    assert!(lock.contains("x86_64-pc-windows-msvc"), "{lock}");

    // The package's sources are in what was published, so a registry or CI can rebuild it.
    let cached = home.join("cache/packages");
    let sources = find_file(&cached, "tiny.c");
    assert!(sources.is_some() || find_file(&home.join("registry"), "tiny.c").is_some());
    let o = velt(
        &user,
        &home,
        &no_zig,
        &["build", "--target", "x86_64-unknown-linux-musl"],
    );
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("no prebuilt native library for x86_64-unknown-linux-musl"),
        "{out}"
    );
    assert!(out.contains("VELT_NATIVE_FROM_SOURCE=1"), "{out}");
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let path = e.path();
        if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        }
    }
    None
}
