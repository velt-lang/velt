//! The fast paths through the `velt` binary: `velt check` (front end only, text and JSON
//! diagnostics, exit codes) and the link step of debug builds (shared runtime when it is built,
//! static with `VELT_RT_LINK=static`, skipped when nothing changed).

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

fn velt(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("VELT_RT_LIB")
        .env_remove("VELT_RT_LINK");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn check_reports_diagnostics_without_building() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(
        dir.join("good.vlt"),
        "function main() {\n  console.log(1);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("bad.vlt"),
        "function main() {\n  const x: number = \"a\";\n}\n",
    )
    .unwrap();

    let o = velt(dir, &["check", "good.vlt"], &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        !dir.join("target").exists(),
        "check must not build anything"
    );

    let o = velt(dir, &["check", "bad.vlt"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("bad.vlt:2:"),
        "{}",
        text(&o.stderr)
    );

    let o = velt(dir, &["check", "bad.vlt", "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout);
    assert!(out.contains(r#""errors":1"#), "{out}");
    assert!(
        out.contains(r#""line":2"#) && out.contains(r#""severity":"error""#),
        "{out}"
    );

    let o = velt(dir, &["check", "missing.vlt", "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stdout).contains(r#""location":null"#));
}

#[test]
fn check_accepts_library_modules_without_main() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(
        dir.join("lib.vlt"),
        "export function double(x: i64): i64 {\n  return x * 2;\n}\n",
    )
    .unwrap();
    // An exported function nothing calls is still type-checked.
    std::fs::write(
        dir.join("badlib.vlt"),
        "export function double(x: i64): i64 {\n  return x * 2;\n}\n\
         export function name(): string {\n  return 1;\n}\n",
    )
    .unwrap();

    let o = velt(dir, &["check", "lib.vlt"], &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(
        (text(&o.stdout), text(&o.stderr)),
        (String::new(), String::new())
    );

    let o = velt(dir, &["check", "lib.vlt", "--json"], &[]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains(r#""errors":0"#), "{out}");
    assert!(!out.contains("main"), "{out}");

    let o = velt(dir, &["check", "badlib.vlt"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    assert!(err.contains("badlib.vlt:5:"), "{err}");
    assert!(!err.contains("`main` function not found"), "{err}");

    let o = velt(dir, &["check", "badlib.vlt", "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o.stdout);
    assert!(out.contains(r#""line":5"#), "{out}");
    assert!(!out.contains("`main` function not found"), "{out}");

    // In a library, `main` may name something other than a function.
    for (file, src) in [
        ("type_main.vlt", "export type main = i64;\n"),
        ("const_main.vlt", "export const main: i64 = 1;\n"),
    ] {
        std::fs::write(dir.join(file), src).unwrap();
        let o = velt(dir, &["check", file], &[]);
        assert!(o.status.success(), "{file}: {}", text(&o.stderr));
    }

    // Building or running a library is still an error.
    for cmd in ["build", "run"] {
        let o = velt(dir, &[cmd, "lib.vlt"], &[]);
        assert_eq!(o.status.code(), Some(1), "velt {cmd}");
        let err = text(&o.stderr);
        assert!(
            err.contains("`main` function not found in the root module"),
            "{err}"
        );
    }
    assert!(!dir.join("target/velt/lib").exists());
}

/// The `link` stage of `velt build -v --timings`, and whether it was skipped.
fn build(dir: &Path, env: &[(&str, &str)]) -> (bool, String) {
    // Links against the runtime libraries: current ones.
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let o = velt(dir, &["build", "hello.vlt", "--timings"], env);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    (err.contains("up to date"), err)
}

#[test]
fn debug_links_are_skipped_when_nothing_changed() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let hello = "function main() {\n  console.log(\"hi\");\n}\n";
    std::fs::write(dir.join("hello.vlt"), hello).unwrap();
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });

    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "first build must link:\n{err}");
    let (skipped, err) = build(dir, &[]);
    assert!(skipped, "unchanged rebuild must not link:\n{err}");

    // Another runtime (the static one) is a different link; without a shared runtime (musl
    // targets cannot build one) the default already is the static one, still up to date.
    let (skipped, err) = build(dir, &[("VELT_RT_LINK", "static")]);
    assert_eq!(skipped, !shared_runtime_built(), "{err}");
    assert_eq!(text(&run_copy(&exe, "hi").stdout), "hi\n");

    // A changed program links again; a deleted executable too.
    std::fs::write(dir.join("hello.vlt"), hello.replace("hi", "ho")).unwrap();
    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "{err}");
    std::fs::remove_file(&exe).unwrap();
    let (skipped, err) = build(dir, &[]);
    assert!(!skipped, "{err}");
    assert_eq!(text(&run_copy(&exe, "ho").stdout), "ho\n");
}

#[test]
fn code_compiled_once_comes_from_the_build_cache() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let cache = cache.to_str().unwrap();
    let main = "import { greet } from \"./lib\";\nfunction main() {\n  greet();\n}\n";
    std::fs::write(dir.join("hello.vlt"), main).unwrap();
    let lib = "export function greet() {\n  console.log(\"hi\");\n}\n";
    std::fs::write(dir.join("lib.vlt"), lib).unwrap();
    // `(found, of)` from the codegen line of `-v`.
    let build = || {
        // Without the build stamp, which would skip compiling an unchanged program.
        let env = [("VELT_BUILD_CACHE", cache), ("VELT_BUILD_STAMP", "off")];
        let o = velt(dir, &["build", "hello.vlt", "-v"], &env);
        let err = text(&o.stderr);
        assert!(o.status.success(), "{err}");
        let line = err
            .lines()
            .find(|l| l.contains("from the build cache"))
            .unwrap_or_else(|| panic!("no cache counts:\n{err}"));
        let counts: Vec<u32> = line
            .split(['(', ' '])
            .filter_map(|w| w.parse().ok())
            .collect();
        assert_eq!(counts.len(), 2, "{line}");
        (counts[0], counts[1])
    };
    let (found, of) = build();
    assert!(
        found < of,
        "a new cache has not seen the program: {found} of {of}"
    );
    let (found, of) = build();
    assert_eq!(
        found, of,
        "compiling again takes every function from the cache"
    );
    // An edit compiles only the functions whose code changed: here `greet`. (Changing only a
    // string would change none: functions refer to their data by a name the key leaves out.)
    let edited = lib.replace(
        "console.log(\"hi\");",
        "console.log(\"ho\");\n  console.log(\"!\");",
    );
    std::fs::write(dir.join("lib.vlt"), edited).unwrap();
    let (found, of) = build();
    assert_eq!(found + 1, of, "{found} of {of}");
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });
    assert_eq!(text(&run_copy(&exe, "cached").stdout), "ho\n!\n");
}

#[test]
fn unchanged_builds_compile_nothing() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let main = "import { greet } from \"./lib\";\nfunction main() {\n  greet();\n}\n";
    std::fs::write(dir.join("hello.vlt"), main).unwrap();
    let lib = "export function greet() {\n  console.log(\"hi\");\n}\n";
    std::fs::write(dir.join("lib.vlt"), lib).unwrap();
    let unchanged = || {
        let o = velt(dir, &["build", "hello.vlt", "-v"], &[]);
        let err = text(&o.stderr);
        assert!(o.status.success(), "{err}");
        (err.contains("nothing to compile"), err)
    };
    assert!(!unchanged().0, "first build");
    let (skipped, err) = unchanged();
    assert!(skipped, "an unchanged build compiles nothing:\n{err}");
    // Saving a file without changing it changes nothing; changing an import does.
    std::fs::write(dir.join("lib.vlt"), lib).unwrap();
    assert!(unchanged().0);
    std::fs::write(dir.join("lib.vlt"), lib.replace("hi", "ho")).unwrap();
    assert!(!unchanged().0);
    assert!(unchanged().0);
    // A new file where imports look is a change too.
    std::fs::write(dir.join("notes.txt"), "").unwrap();
    assert!(!unchanged().0);
    let off = velt(
        dir,
        &["build", "hello.vlt", "-v"],
        &[("VELT_BUILD_STAMP", "off")],
    );
    assert!(!text(&off.stderr).contains("nothing to compile"));
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });
    assert_eq!(text(&run_copy(&exe, "stamped").stdout), "ho\n");
}

#[test]
fn a_package_nearer_the_importer_is_a_change() {
    let tmp = test_dir::TestDir::new();
    let ws = tmp.path();
    let write = |rel: &str, text: &str| {
        let path = ws.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let package = |dir: &str, name: &str, says: &str| {
        write(
            &format!("{dir}/package.json"),
            &format!("{{ \"name\": \"{name}\" }}"),
        );
        let src = format!("export function say(): string {{\n  return \"{says}\";\n}}\n");
        write(&format!("{dir}/index.ts"), &src);
    };
    package("node_modules/b", "b", "outer b");
    write("p/package.json", "{ \"name\": \"p\" }");
    package("p/node_modules/a", "a", "a");
    let main = "import { say as a } from \"a\";\nimport { say as b } from \"b\";\n\nconsole.log(a());\nconsole.log(b());\n";
    write("p/main.ts", main);
    let run = || {
        let o = velt(&ws.join("p"), &["run", "main.ts"], &[]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        text(&o.stdout)
    };
    assert_eq!(run(), "a\nouter b\n");
    // Module resolution looked for `b` in `p/node_modules` before `node_modules`.
    package("p/node_modules/b", "b", "INNER b");
    assert_eq!(run(), "a\nINNER b\n");
}

/// A build that writes the program or its object without a stamp (the stamp off, `--report`,
/// `--emit obj` onto the stamped object) leaves no stamp the next build could trust: the
/// program must always be built from the sources as they are.
#[test]
fn outputs_written_without_a_stamp_are_a_change() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let main = "import { word } from \"./lib\";\nfunction main() {\n  console.log(word());\n}\n";
    std::fs::write(dir.join("main.vlt"), main).unwrap();
    let lib = |w: &str| {
        let src = format!("export function word(): string {{\n  return \"{w}\";\n}}\n");
        std::fs::write(dir.join("lib.vlt"), src).unwrap();
    };
    let build = |args: &[&str], env: &[(&str, &str)]| {
        let o = velt(dir, args, env);
        assert!(o.status.success(), "{}", text(&o.stderr));
    };
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "main.exe" } else { "main" });
    let object = || {
        let entries = std::fs::read_dir(dir.join("target/velt")).unwrap();
        (entries.flatten().map(|e| e.path()))
            .find(|p| {
                let ext = p.extension().unwrap_or_default();
                p.file_stem().unwrap_or_default() == "main" && (ext == "o" || ext == "obj")
            })
            .unwrap()
    };
    type Build<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);
    let middle: [Build; 2] = [
        (&["build", "main.vlt"], &[("VELT_BUILD_STAMP", "off")]),
        (&["build", "main.vlt", "--report", "numbers"], &[]),
    ];
    for (i, (args, env)) in middle.iter().enumerate() {
        lib("BBBB");
        build(&["build", "main.vlt"], &[]);
        lib("AAAA");
        build(args, env);
        lib("BBBB");
        build(&["build", "main.vlt"], &[]);
        let out = run_copy(&exe, &format!("case{i}"));
        assert_eq!(text(&out.stdout), "BBBB\n", "case {i}");
    }
    // An object written by `--emit obj` where the stamped build keeps its own.
    lib("BBBB");
    build(&["build", "main.vlt"], &[]);
    let obj = object();
    lib("AAAA");
    let obj_arg = obj.to_str().unwrap();
    build(&["build", "main.vlt", "--emit", "obj", "-o", obj_arg], &[]);
    lib("BBBB");
    build(&["build", "main.vlt"], &[]);
    assert_eq!(text(&run_copy(&exe, "obj").stdout), "BBBB\n");
}

#[test]
fn cache_warm_compiles_the_warm_up_programs_once() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let warm = dir.join("share/warm");
    std::fs::create_dir_all(&warm).unwrap();
    let program = "function main() {\n  const m = new Map<string, number>();\n  m.set(\"a\", 1);\n  console.log(m.get(\"a\"));\n}\n";
    std::fs::write(warm.join("map.vlt"), program).unwrap();
    let cache = dir.join("cache");
    let env = [
        (
            "VELT_SHARE",
            dir.join("share").to_str().unwrap().to_string(),
        ),
        ("VELT_BUILD_CACHE", cache.to_str().unwrap().to_string()),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let o = velt(dir, &["cache", "warm"], &env);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("warmed the build cache with 1 programs"),
        "{err}"
    );
    let again = velt(dir, &["cache", "warm"], &env);
    assert!(text(&again.stderr).contains("warm for this velt already"));

    // A build of the same code then takes every function from the cache.
    std::fs::write(dir.join("app.vlt"), program).unwrap();
    let mut build_env = env.clone();
    build_env.push(("VELT_BUILD_STAMP", "off"));
    let o = velt(
        dir,
        &["build", "app.vlt", "-v", "--cpu", "native"],
        &build_env,
    );
    let err = text(&o.stderr);
    let line = err.lines().find(|l| l.contains("from the build cache"));
    let counts: Vec<u32> = line
        .unwrap_or_else(|| panic!("{err}"))
        .split(['(', ' '])
        .filter_map(|w| w.parse().ok())
        .collect();
    assert!(counts.len() == 2 && counts[0] == counts[1], "{err}");

    // A program that does not compile is reported, and leaves the cache unmarked.
    std::fs::remove_dir_all(&cache).unwrap();
    std::fs::write(warm.join("broken.vlt"), "function main() {\n  nope();\n}\n").unwrap();
    let o = velt(dir, &["cache", "warm"], &env);
    let err = text(&o.stderr);
    assert!(!o.status.success() && err.contains("broken.vlt"), "{err}");
    let o = velt(dir, &["cache", "warm"], &env);
    assert!(text(&o.stderr).contains("warmed the build cache with 2 programs"));
}

/// Run a copy of `exe` named after `tag`, beside it (where Windows finds the runtime DLL). The
/// test relinks `exe` afterwards, and on Windows a program that has just exited can keep its file
/// locked for a moment (an antivirus scan), so the relink must never target a file that ran.
fn run_copy(exe: &Path, tag: &str) -> Output {
    let mut name = exe.file_stem().unwrap().to_os_string();
    name.push(format!("-{tag}"));
    let copy = exe
        .with_file_name(name)
        .with_extension(exe.extension().unwrap_or_default());
    std::fs::copy(exe, &copy).unwrap();
    crate::no_window::command(&copy).output().unwrap()
}

/// Whether the shared runtime sits next to the `velt` under test (debug links then use it).
fn shared_runtime_built() -> bool {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let built = Path::new(env!("CARGO_BIN_EXE_velt")).parent().unwrap();
    built
        .join(velt_link::shared_runtime_lib_name(
            velt_link::TargetOs::host(),
        ))
        .is_file()
}

#[test]
fn debug_builds_use_the_shared_runtime_when_built() {
    if !shared_runtime_built() {
        eprintln!("skipping: the shared runtime is not built (cargo build -p velt_rt_shared)");
        return;
    }
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(
        dir.join("hello.vlt"),
        "function main() {\n  console.log(\"shared\");\n}\n",
    )
    .unwrap();
    let (_, err) = build(dir, &[]);
    let entry = if cfg!(windows) {
        "hello.entry.obj"
    } else {
        "hello.entry.o"
    };
    assert!(dir.join("target/velt").join(entry).is_file(), "{err}");
    let exe = dir
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });
    let run = crate::no_window::command(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "shared\n", "{}", text(&run.stderr));
}
