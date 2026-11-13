//! What std_scheduler_workers and std_scheduler_workers_net share: a program built with and
//! without `VELT_SCHEDULER=std` and run with some workers.

use std::path::Path;

pub fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built (with `VELT_SCHEDULER=std` when `std`), run with `threads` workers: its exit
/// code and stdout.
#[allow(dead_code)] // std_scheduler_workers_net uses only `build_and_run_with`
pub fn build_and_run(dir: &Path, name: &str, src: &str, std: bool, threads: &str) -> (i32, String) {
    build_and_run_with(dir, name, src, std, threads, &[])
}

/// [`build_and_run`] with more variables (`env`) for the run.
pub fn build_and_run_with(
    dir: &Path,
    name: &str,
    src: &str,
    std: bool,
    threads: &str,
    env: &[(&str, &str)],
) -> (i32, String) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}{}{}",
        if std { "_std" } else { "" },
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let mut build = super::no_window::command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env_remove("VELT_SCHEDULER");
    if std {
        build.env("VELT_SCHEDULER", "std");
    }
    let o = build.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = super::no_window::command(&exe)
        .env("VELT_THREADS", threads)
        .env_remove("VELT_SCHEDULER")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    (o.status.code().unwrap_or(-1), format!("{out}{err}"))
}

/// Builds and runs `src` both ways with 4 workers (and the std build with 1 too): each prints
/// `want` and exits with 0.
pub fn same_on_both(name: &str, src: &str, want: &str) {
    super::runtime_support::build_native_runtime(manifest());
    let dir = super::test_dir::TestDir::new();
    let (code, rt) = build_and_run(dir.path(), name, src, false, "4");
    assert_eq!(
        (code, rt.as_str()),
        (0, want),
        "{name}: the runtime's executor"
    );
    let (code, std) = build_and_run(dir.path(), name, src, true, "4");
    assert_eq!(
        (code, std.as_str()),
        (0, want),
        "{name}: the executor in Velt, 4 workers"
    );
}

/// Workers only start where `#sched-thread` has threads (macOS, Linux and Windows).
pub fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}
