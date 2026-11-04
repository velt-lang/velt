//! Which clang release builds use, end to end, with scripts standing in for clang: they answer
//! `--version` themselves and hand everything else to the real one.
#![cfg(unix)]

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

mod no_window;
mod runtime_support;
mod test_dir;

const HELLO: &str = "function main() {\n  console.log(\"hello\");\n}\n";
/// A program whose std code the std cache serves.
const STD_PROGRAM: &str =
    "function main() {\n  console.log(new URL(\"https://a.example/b\").host);\n}\n";

/// Write the script `path`, a clang that reports `banner` and logs each compile to `$CLANG_LOG`.
fn fake_clang(path: &Path, banner: &str, real: &Path) {
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"{banner}\"; exit 0; fi\n\
         echo \"$0\" >> \"$CLANG_LOG\"\nexec \"{}\" \"$@\"\n",
        real.display()
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Write the script `path`, which runs `body`.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A wrapper as ccache's or asdf's are: it starts `real`, beside it, for everything.
fn wrapper(path: &Path) {
    script(path, "exec \"$(dirname \"$0\")/real\" \"$@\"");
}

/// `dirs` before this machine's `PATH`.
fn path_with(dirs: &[&Path]) -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let rest = std::env::split_paths(&path);
    std::env::join_paths(dirs.iter().map(PathBuf::from).chain(rest)).unwrap()
}

/// `velt <args>` in `dir` with this `PATH` and no `$VELT_CLANG`.
fn velt_on_path(dir: &Path, args: &[&str], log: &Path, path: &std::ffi::OsStr) -> Output {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("CLANG_LOG", log)
        .env("PATH", path)
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env_remove("VELT_CLANG")
        .env_remove("VELT_RT_LINK")
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap()
}

const RELEASE_BUILD: [&str; 5] = ["build", "a.vlt", "--release", "-o", "a"];

/// The real clang; tests that need one pass when there is none, as the other LLVM tests do.
fn real_clang() -> Option<PathBuf> {
    velt_codegen_llvm::find_clang()
}

/// `velt <args>` in `dir`, with clang's compiles logged to `log`.
fn velt(dir: &Path, args: &[&str], log: &Path, env: &[(&str, &Path)]) -> Output {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd: Command = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .current_dir(dir)
        .env("CLANG_LOG", log)
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env_remove("VELT_RT_LINK")
        .env_remove("VELT_STD_OBJECT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// How many compiles were logged since the last call.
fn compiles(log: &Path) -> usize {
    let lines = std::fs::read_to_string(log).map_or(0, |t| t.lines().count());
    let _ = std::fs::remove_file(log);
    lines
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Two clangs that differ only in the version they report, as Homebrew's after an upgrade: `link`
/// is the path the build sees (`opt/llvm`), and the files have one size and one time, so only the
/// version tells them apart.
struct TwoClangs {
    link: PathBuf,
    old: PathBuf,
    new: PathBuf,
}

impl TwoClangs {
    fn new(dir: &Path, real: &Path) -> TwoClangs {
        let (old, new) = (dir.join("23.1.4"), dir.join("23.1.5"));
        let time = SystemTime::now() - Duration::from_secs(3600);
        for (version, cellar) in [("23.1.4", &old), ("23.1.5", &new)] {
            std::fs::create_dir_all(cellar).unwrap();
            let clang = cellar.join("clang");
            fake_clang(&clang, &format!("clang version {version}"), real);
            let file = std::fs::File::options().write(true).open(&clang).unwrap();
            file.set_modified(time).unwrap();
        }
        let two = TwoClangs {
            link: dir.join("clang"),
            old: old.join("clang"),
            new: new.join("clang"),
        };
        let (a, b) = (two.old.metadata().unwrap(), two.new.metadata().unwrap());
        assert_eq!(a.len(), b.len());
        assert_eq!(a.modified().unwrap(), b.modified().unwrap());
        two.point_at(&two.old);
        two
    }

    fn point_at(&self, clang: &Path) {
        let _ = std::fs::remove_file(&self.link);
        symlink(clang, &self.link).unwrap();
    }
}

/// A new clang behind the same path, size and time compiles again: neither the build stamp nor the
/// object cache serves what the old one made.
#[test]
fn a_new_clang_does_not_serve_the_old_ones_objects() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let clangs = TwoClangs::new(dir, &real);
    let log = dir.join("log");
    let build = || {
        let args = ["build", "a.vlt", "--release", "-o", "a"];
        let o = velt(dir, &args, &log, &[("VELT_CLANG", &clangs.link)]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        compiles(&log)
    };
    assert!(build() > 0, "the first build compiles");
    assert_eq!(build(), 0, "unchanged: nothing to compile");
    clangs.point_at(&clangs.new);
    assert!(build() > 0, "a new clang compiles again");
    assert_eq!(build(), 0, "and is then known");
    // The old objects are still there for the old clang: the key told them apart.
    clangs.point_at(&clangs.old);
    assert_eq!(build(), 0, "back to the old clang: its objects are served");
}

/// The same for the std functions cached across programs (`VELT_STD_OBJECT`).
#[test]
fn a_new_clang_does_not_serve_the_old_ones_std_functions() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), STD_PROGRAM).unwrap();
    let clangs = TwoClangs::new(dir, &real);
    let log = dir.join("log");
    let on = Path::new("1");
    let build = || {
        let env = [
            ("VELT_CLANG", clangs.link.as_path()),
            ("VELT_STD_OBJECT", on),
            ("VELT_STD_OBJECT_STATS", on),
        ];
        let o = velt(dir, &["build", "a.vlt", "--release", "-o", "a"], &log, &env);
        let err = text(&o.stderr);
        assert!(o.status.success(), "{err}");
        let line = (err.lines())
            .find(|l| l.contains("found,"))
            .unwrap_or_else(|| panic!("no stats in\n{err}"));
        let n: Vec<usize> = line
            .split([' ', ','])
            .filter_map(|w| w.parse().ok())
            .collect();
        (n[0], n[1])
    };
    let (_, compiled) = build();
    assert!(compiled > 0, "the first build compiles std functions");
    clangs.point_at(&clangs.new);
    let (found, again) = build();
    assert_eq!(
        (found, again),
        (0, compiled),
        "a new clang compiles them all"
    );
}

/// A wrapper stays the same while the clang behind it changes: the new clang's version must reach
/// the keys, so a wrapper's version is not remembered.
#[test]
fn a_new_clang_behind_a_wrapper_compiles_again() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let clang = dir.join("clang-29");
    wrapper(&clang);
    fake_clang(&dir.join("real"), "clang version 97.0.0", &real);
    let log = dir.join("log");
    let build = || {
        let o = velt(dir, &RELEASE_BUILD, &log, &[("VELT_CLANG", &clang)]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        compiles(&log)
    };
    assert!(build() > 0, "the first build compiles");
    assert_eq!(build(), 0, "unchanged: nothing to compile");
    fake_clang(&dir.join("real"), "clang version 98.0.0", &real);
    assert!(build() > 0, "a new clang behind it compiles again");
    let o = velt(dir, &["doctor"], &log, &[("VELT_CLANG", &clang)]);
    let out = text(&o.stdout);
    assert!(out.contains("(clang 98.0.0): set by"), "{out}");
}

/// What a wrapper starts has gone: it no longer counts, and the build goes to the next clang.
#[test]
fn a_clang_that_broke_after_a_build_falls_back_to_the_next() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    wrapper(&bin.join("clang"));
    fake_clang(&bin.join("real"), "clang version 98.0.0", &real);
    let log = dir.join("log");
    let path = path_with(&[&bin]);
    let o = velt_on_path(dir, &RELEASE_BUILD, &log, &path);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(compiles(&log) > 0, "the wrapper compiled");
    std::fs::remove_file(bin.join("real")).unwrap();
    std::fs::write(dir.join("a.vlt"), HELLO.replace("hello", "again")).unwrap();
    let o = velt_on_path(dir, &RELEASE_BUILD, &log, &path);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&velt_on_path(dir, &["doctor"], &log, &path).stdout);
    let bad = format!("{} (clang 98", bin.join("clang").display());
    assert!(!out.contains(&bad), "{out}");
}

/// `--version` answers but nothing compiles (a toolchain for another target only): the next clang
/// is used, and `velt doctor` says why. Then the build compiles nothing with the first.
#[test]
fn a_clang_that_compiles_nothing_falls_back_to_the_next() {
    let Some(_) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let clang = bin.join("clang");
    script(
        &clang,
        "if [ \"$1\" = --version ]; then echo clang version 98.0.0; exit 0; fi\n\
         echo \"error: no available targets\" >&2\nexit 1",
    );
    let log = dir.join("log");
    let path = path_with(&[&bin]);
    let o = velt_on_path(dir, &RELEASE_BUILD, &log, &path);
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&velt_on_path(dir, &["doctor"], &log, &path).stdout);
    let want = format!(
        "{} (clang 98.0.0) can't compile for this machine: error: no available targets",
        clang.display()
    );
    assert!(out.contains(&want), "{out}");
}

/// A clang that compiles nothing isn't reported as the compiler's bug, and the message names it
/// and how to choose another. `$VELT_CLANG` is final, so here the build does fail.
#[test]
fn a_clang_that_compiles_nothing_is_named_not_called_a_compiler_bug() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let clang = dir.join("my-clang");
    script(
        &clang,
        "if [ \"$1\" = --version ]; then echo clang version 98.0.0; exit 0; fi\n\
         echo \"error: no available targets\" >&2\nexit 1",
    );
    let log = dir.join("log");
    let o = velt(dir, &RELEASE_BUILD, &log, &[("VELT_CLANG", &clang)]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(!err.contains("compiler bug"), "{err}");
    assert!(err.contains(&clang.display().to_string()), "{err}");
    assert!(err.contains("clang version 98.0.0"), "{err}");
    assert!(err.contains("no available targets"), "{err}");
    assert!(err.contains("$VELT_CLANG"), "{err}");
}

/// The same for a clang that won't start at all: what it starts has gone.
#[test]
fn a_clang_that_will_not_start_is_named_not_called_a_compiler_bug() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let clang = dir.join("my-clang");
    script(
        &clang,
        "if [ \"$1\" = --version ]; then echo clang version 98.0.0; exit 0; fi\n\
         exec /nonexistent/clang \"$@\"",
    );
    let log = dir.join("log");
    let o = velt(dir, &RELEASE_BUILD, &log, &[("VELT_CLANG", &clang)]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(!err.contains("compiler bug"), "{err}");
    assert!(err.contains(&clang.display().to_string()), "{err}");
    assert!(err.contains("$VELT_CLANG"), "{err}");
}

/// A clang that compiles small IR and rejects the program's is what the message was for.
#[test]
fn a_clang_that_rejects_only_the_generated_ir_is_a_compiler_bug() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let clang = dir.join("my-clang");
    script(
        &clang,
        "if [ \"$1\" = --version ]; then echo clang version 98.0.0; exit 0; fi\n\
         input=$(cat)\n\
         case \"$input\" in *velt_clang_check*) exit 0;; esac\n\
         echo \"error: expected instruction opcode\" >&2\nexit 1",
    );
    let log = dir.join("log");
    let o = velt(dir, &RELEASE_BUILD, &log, &[("VELT_CLANG", &clang)]);
    assert!(!o.status.success());
    let err = text(&o.stderr);
    assert!(err.contains("this is a compiler bug"), "{err}");
    assert!(err.contains("expected instruction opcode"), "{err}");
}

/// A file of clang's name that can't be started doesn't hide the next on `PATH`: `execvp` passes it
/// by, so the probe does, and what the build runs must be what the probe asked.
#[test]
fn a_file_that_cannot_start_does_not_hide_the_clang_behind_it() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    std::fs::write(dir.join("a.vlt"), HELLO).unwrap();
    let (first, second) = (dir.join("first"), dir.join("second"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(first.join("clang"), "#!/bin/sh\nexit 1\n").unwrap();
    fake_clang(&second.join("clang"), "clang version 97.0.0", &real);
    let log = dir.join("log");
    let path = path_with(&[&first, &second]);
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(RELEASE_BUILD)
        .current_dir(dir)
        .env("CLANG_LOG", &log)
        .env("PATH", path)
        .env("VELT_CLANG", "clang")
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env_remove("VELT_RT_LINK")
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(compiles(&log) > 0, "the second clang compiled");
}

/// `velt doctor` names the choice and the reason, and a release build compiles with that clang.
#[test]
fn doctor_says_it_was_set_by_velt_clang() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let clang = dir.join("my-clang");
    fake_clang(&clang, "clang version 97.1.2", &real);
    let log = dir.join("log");
    let o = velt(dir, &["doctor"], &log, &[("VELT_CLANG", &clang)]);
    let out = text(&o.stdout);
    let want = format!("{} (clang 97.1.2): set by $VELT_CLANG", clang.display());
    assert!(out.contains(&want), "{out}");
    assert!(compiles(&log) > 0, "the smoke test used it:\n{out}");
}

/// `dir`'s scripts first on `PATH`: `clang-30` is too old, `clang-28` does not run, and `clang-29`
/// reports a version no real clang has yet, so it is the newest whatever else this machine has.
fn doctor_with_fakes(dir: &Path, real: &Path, log: &Path) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_clang(&bin.join("clang-30"), "clang version 14.0.0", real);
    fake_clang(&bin.join("clang-29"), "clang version 98.0.0", real);
    let broken = bin.join("clang-28");
    std::fs::write(&broken, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake_clang(&bin.join("clang"), "clang version 97.0.0", real);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("doctor")
        .current_dir(dir)
        .env("CLANG_LOG", log)
        .env("PATH", path)
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env_remove("VELT_CLANG")
        .env_remove("VELT_RT_LINK")
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap();
    text(&o.stdout)
}

/// The newest clang wins over `clang` on `PATH`; the others are named with why they lost.
#[cfg(target_os = "macos")]
#[test]
fn macos_prefers_the_newest_clang() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let log = dir.join("log");
    let out = doctor_with_fakes(dir, &real, &log);
    let bin = dir.join("bin");
    let line = (out.lines())
        .find(|l| l.contains(" clang "))
        .unwrap_or_else(|| panic!("no clang line:\n{out}"));
    let want = format!(
        "{} (clang 98.0.0): the newest clang found",
        bin.join("clang-29").display()
    );
    assert!(line.contains(&want), "{line}");
    assert!(line.contains("is clang 14.0.0, but"), "{line}");
    assert!(line.contains("clang-28` does not run"), "{line}");
    assert!(
        line.contains(&format!("also found {}", bin.join("clang").display())),
        "{line}"
    );
    let used = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        used.lines().any(|l| l.ends_with("clang-29")),
        "{used}\n{out}"
    );
    assert!(
        used.lines().all(|l| l.ends_with("clang-29")),
        "{used}\n{out}"
    );
}

/// Elsewhere the search order decides: `clang` on `PATH`, however new a `clang-NN` beside it.
#[cfg(not(target_os = "macos"))]
#[test]
fn elsewhere_the_first_usable_clang_is_kept() {
    let Some(real) = real_clang() else { return };
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let log = dir.join("log");
    let out = doctor_with_fakes(dir, &real, &log);
    let want = format!(
        "{} (clang 97.0.0): the first usable clang in the search order",
        dir.join("bin/clang").display()
    );
    assert!(out.contains(&want), "{out}");
}
