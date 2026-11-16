//! What std_scheduler_workers and std_scheduler_workers_net share: a program built with
//! `VELT_SCHEDULER=std` and run with some workers.

use std::path::Path;

pub fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built with `VELT_SCHEDULER=std`, run with `threads` workers: its exit code and stdout.
#[allow(dead_code)] // std_scheduler_workers_net uses only `build_and_run_with`
pub fn build_and_run(dir: &Path, name: &str, src: &str, threads: &str) -> (i32, String) {
    build_and_run_with(dir, name, src, threads, &[])
}

/// [`build_and_run`] with more variables (`env`) for the run.
pub fn build_and_run_with(
    dir: &Path,
    name: &str,
    src: &str,
    threads: &str,
    env: &[(&str, &str)],
) -> (i32, String) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = super::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
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

/// Builds and runs `src` with 4 workers: it prints `want` and exits with 0.
#[allow(dead_code)] // std_scheduler_join_order uses none of the plain runs
pub fn prints(name: &str, src: &str, want: &str) {
    super::runtime_support::build_native_runtime(manifest());
    let dir = super::test_dir::TestDir::new();
    let (code, std) = build_and_run(dir.path(), name, src, "4");
    assert_eq!((code, std.as_str()), (0, want), "{name}: 4 workers");
}

/// Workers only start where `#sched-thread` has threads (macOS, Linux and Windows).
pub fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// Runs the std build of `name` (built in `dir`) with `threads` workers: on Linux 40 times with the
/// program and two spinning threads on cores 0 and 1 (`sched_setaffinity`), elsewhere 10 plain
/// runs (macOS cannot pin threads). The runs that did not print `want` with exit code 0.
#[allow(dead_code)] // std_scheduler_workers_net uses none of these
pub fn contended_runs(dir: &Path, name: &str, threads: &str, want: &str) -> Vec<String> {
    contended_runs_checked(dir, name, threads, |out| {
        if out == want {
            Ok(())
        } else {
            Err(format!("{out:?}"))
        }
    })
}

/// [`contended_runs`] with `check` judging each run's stdout (exit code 0 required too): the runs
/// it rejected, with its reason.
#[allow(dead_code)]
pub fn contended_runs_checked(
    dir: &Path,
    name: &str,
    threads: &str,
    check: impl Fn(&str) -> Result<(), String>,
) -> Vec<String> {
    contended_runs_spun(dir, name, threads, Spin::Threads, check)
}

/// What spins on cores 0 and 1 while [`contended_runs_spun`] runs a program there.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Spin {
    /// Two threads of the test process.
    Threads,
    /// Two processes of their own (`sh`). The program's threads start later behind these: the
    /// end of #3076's program was lost in 17 runs of 80 with them, in none of 160 with threads.
    Processes,
}

/// [`contended_runs_checked`] with `spin` spinning on the two cores (Linux only).
#[allow(dead_code)]
pub fn contended_runs_spun(
    dir: &Path,
    name: &str,
    threads: &str,
    spin: Spin,
    check: impl Fn(&str) -> Result<(), String>,
) -> Vec<String> {
    let mut spinners = Spinners(Vec::new());
    if cfg!(target_os = "linux") && spin == Spin::Processes {
        for _ in 0..2 {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", "while :; do :; done"]);
            pin_child(&mut c);
            spinners.0.push(c.spawn().expect("sh"));
        }
    }
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let linux = cfg!(target_os = "linux");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let burners: Vec<_> = (0..if linux && spin == Spin::Threads { 2 } else { 0 })
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                pin_to_two_cores();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    let mut lost = Vec::new();
    for run in 0..if linux { 40 } else { 10 } {
        let mut cmd = super::no_window::command(&exe);
        cmd.env("VELT_THREADS", threads)
            .env_remove("VELT_SCHEDULER");
        pin_child(&mut cmd);
        let o = cmd.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        match (o.status.code(), check(&out)) {
            (Some(0), Ok(())) => {}
            (code, Err(why)) => lost.push(format!("run {run}: {code:?} {why}")),
            (code, Ok(())) => lost.push(format!("run {run}: exit {code:?}")),
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for b in burners {
        b.join().unwrap();
    }
    drop(spinners);
    lost
}

/// The spinning processes of [`Spin::Processes`], stopped when dropped (a failing test too).
#[allow(dead_code)]
struct Spinners(Vec<std::process::Child>);

impl Drop for Spinners {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Pins the child `cmd` starts to cores 0 and 1 (Linux only).
#[allow(dead_code, unused_variables)]
fn pin_child(cmd: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only an affinity call between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                pin_to_two_cores();
                Ok(())
            });
        }
    }
}

/// Pins the calling thread (or a child between fork and exec) to cores 0 and 1 (Linux only).
#[allow(dead_code)]
fn pin_to_two_cores() {
    #[cfg(target_os = "linux")]
    // SAFETY: a zeroed `cpu_set_t` filled by the libc macros, passed with its size.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(0, &mut set);
        libc::CPU_SET(1, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}
