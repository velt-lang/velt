//! The installed toolchain layout (`<prefix>/bin/velt`, `<prefix>/lib/<runtime libs>`,
//! `<prefix>/std`) works without any `VELT_*` variables: the debug `velt` binary, runtime library
//! and std are copied into a temp prefix, then `velt doctor` and `velt run` run from there.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

const RUNTIME_LIB: &str = if cfg!(windows) {
    "velt_rt.lib"
} else {
    "libvelt_rt.a"
};
/// The shared runtime debug builds link: the library (and on Windows its import library).
const SHARED_RUNTIME: &[&str] = if cfg!(windows) {
    &["velt_rt_shared.dll", "velt_rt_shared.dll.lib"]
} else if cfg!(target_os = "macos") {
    &["libvelt_rt_shared.dylib"]
} else {
    &["libvelt_rt_shared.so"]
};
const EXE: &str = if cfg!(windows) { "velt.exe" } else { "velt" };

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

/// Build `<root>/prefix` from the cargo build outputs; `None` if the runtime library is missing.
fn install(root: &Path) -> Option<PathBuf> {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_velt"));
    let runtime = built.parent()?.join(RUNTIME_LIB);
    if !runtime.is_file() {
        eprintln!(
            "skipping: {} not built (cargo build -p velt_rt)",
            runtime.display()
        );
        return None;
    }
    let prefix = root.join("prefix");
    for dir in ["bin", "lib", "std"] {
        std::fs::create_dir_all(prefix.join(dir)).unwrap();
    }
    std::fs::copy(&built, prefix.join("bin").join(EXE)).unwrap();
    std::fs::copy(&runtime, prefix.join("lib").join(RUNTIME_LIB)).unwrap();
    for name in SHARED_RUNTIME {
        let lib = built.parent()?.join(name);
        if lib.is_file() {
            std::fs::copy(&lib, prefix.join("lib").join(name)).unwrap();
        }
    }
    let share = prefix.join("share/velt/lldb");
    std::fs::create_dir_all(&share).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::copy(
        repo.join("editors/lldb/velt_lldb.py"),
        share.join("velt_lldb.py"),
    )
    .unwrap();
    let repo_std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    if repo_std.is_dir() {
        copy_tree(&repo_std, &prefix.join("std"));
    }
    Some(prefix)
}

fn velt(prefix: &Path, cwd: &Path, home: &Path, args: &[&str]) -> Output {
    crate::no_window::command(prefix.join("bin").join(EXE))
        .args(args)
        .current_dir(cwd)
        .env("VELT_HOME", home)
        .env_remove("VELT_STD")
        .env_remove("VELT_SHARE")
        .env_remove("VELT_RT_LIB")
        .env_remove("VELT_RT_LINK")
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_LAUNCHER")
        .env_remove("VELT_TOOLCHAIN_SELECTED")
        .output()
        .unwrap()
}

#[test]
fn installed_prefix_runs_doctor_and_programs() {
    let tmp = test_dir::TestDir::new();
    let Some(prefix) = install(tmp.path()) else {
        return;
    };
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&work).unwrap();

    let o = velt(&prefix, &work, &home, &["doctor"]);
    let report = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "doctor failed:\n{report}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let lib = prefix.join("lib").join(RUNTIME_LIB);
    let std = prefix.join("std");
    assert!(
        report.contains(&lib.display().to_string()),
        "runtime lib not from the prefix:\n{report}"
    );
    assert!(
        report.contains(&std.display().to_string()),
        "std not from the prefix:\n{report}"
    );
    let script = prefix.join("share/velt/lldb/velt_lldb.py");
    assert!(
        report.contains(&script.display().to_string()),
        "debugger scripts not from the prefix:\n{report}"
    );
    assert!(report.contains("✓ hello (debug)"), "{report}");
    // Not run by the launcher (#948): doctor says so, and `velt toolchain` points at it.
    assert!(
        report.contains("toolchain") && report.contains("started directly"),
        "{report}"
    );
    let o = velt(&prefix, &work, &home, &["toolchain", "list"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("is run by the velt launcher"),
        "{err}"
    );
    if prefix.join("lib").join(SHARED_RUNTIME[0]).is_file() {
        let shared = prefix.join("lib").join(SHARED_RUNTIME.last().unwrap());
        assert!(
            report.contains(&shared.display().to_string()),
            "shared runtime not from the prefix:\n{report}"
        );
    }

    std::fs::write(
        work.join("hello.vlt"),
        "function main() {\n  console.log(\"installed ok\");\n}\n",
    )
    .unwrap();
    let o = velt(&prefix, &work, &home, &["run", "hello.vlt"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "installed ok\n",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let exe = work
        .join("target/velt")
        .join(if cfg!(windows) { "hello.exe" } else { "hello" });
    assert!(exe.is_file());

    // What the VS Code extension asks for: the program and the LLDB script, as absolute paths.
    let o = velt(&prefix, &work, &home, &["build", "hello.vlt", "--json"]);
    let out: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(out["errors"], 0, "{out}");
    assert_eq!(out["debugInfo"], true, "{out}");
    let same = |key: &str, path: &Path| {
        let got = Path::new(out[key].as_str().unwrap_or_else(|| panic!("{out}")));
        assert_eq!(
            got.canonicalize().unwrap(),
            path.canonicalize().unwrap(),
            "{key}: {out}"
        );
    };
    same("executable", &exe);
    same("lldbScript", &script);

    // The runtime's build profile: doctor names it, and a release build warns about a debug one
    // (cargo's debug runtime here, unless the tests run with `--release`).
    let debug_runtime = cfg!(debug_assertions);
    let profile = if debug_runtime {
        "(debug build"
    } else {
        "(release build)"
    };
    assert!(report.contains(profile), "no `{profile}` in:\n{report}");
    // From another directory, so the release link writes a new executable instead of the debug
    // one that just ran (on Windows a just-run executable can stay locked for a moment).
    let release = work.join("release");
    std::fs::create_dir_all(&release).unwrap();
    let o = velt(
        &prefix,
        &release,
        &home,
        &["run", "--release", "../hello.vlt"],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "installed ok\n");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(
        stderr.contains("linked a debug build of the runtime"),
        debug_runtime,
        "{stderr}"
    );
}
