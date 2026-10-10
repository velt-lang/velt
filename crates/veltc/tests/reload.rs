//! Reload goldens: drive `velt dev` through `tests/reload/<case>/<step>/` and check what the
//! running program does after each step.
//!
//! Each step directory holds the files to write into the program directory (step 1: the whole
//! program, later steps: the files that change) and an `expect` file with lines in order:
//! - `action: start|swap|restart|keep|exit`: the supervisor started the program, hot-swapped
//!   the change into it (JIT mode; `--exe` mode restarts instead), replaced it after a good
//!   build, kept the old version after a failed build, or saw the program exit;
//! - `reason: <text>`: (JIT mode) the restart said `restarted (<text>)`;
//! - `get <path>: <body>`: an HTTP GET to the program (its port comes from its
//!   `listening on <port>` line) answers `<body>`;
//! - `background: <path>`: start a GET that stays in flight across the next step's reload;
//! - `await <path>: <body>`: that GET answers `<body>`, after this step's action happened;
//! - `stdout: <line>`: the program printed `<line>` during this step.
//!
//! A line starting with `jit ` or `exe ` applies to that mode only (state that survives a hot
//! swap starts over on a restart). Every case runs in both `velt dev` modes (JIT host and
//! `--exe`) on every platform. For servers a client thread keeps connecting throughout; since
//! the supervisor hands the listening socket over, no connection may be refused. Filter:
//! `VELT_RELOAD=<case>`. `bench_save_to_first_response` (ignored) times save → first new
//! response.

mod no_window;
mod reload_support;
mod runtime_support;
mod test_dir;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reload_support::{get, Background, Dev, Mark, Probe};
use test_dir::TestDir;

/// One `expect` line.
#[derive(Debug)]
enum Expect {
    Action(String),
    Reason(String),
    Get(String, String),
    Background(String),
    Await(String, String),
    Stdout(String),
}

/// The two `velt dev` modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Jit,
    Exe,
}

impl Mode {
    fn flags(self) -> &'static [&'static str] {
        match self {
            Mode::Jit => &[],
            Mode::Exe => &["--exe"],
        }
    }
}

/// `expect` lines that apply in `mode`.
fn parse_expect(text: &str, mode: Mode) -> Vec<Expect> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let (key, value) = line
                .split_once(": ")
                .unwrap_or_else(|| panic!("bad expect line `{line}`"));
            let (only, key) = match key.split_once(' ') {
                Some(("jit", rest)) => (Some(Mode::Jit), rest),
                Some(("exe", rest)) => (Some(Mode::Exe), rest),
                _ => (None, key),
            };
            if only.is_some_and(|m| m != mode) {
                return None;
            }
            Some(parse_line(line, key, value))
        })
        .collect()
}

fn parse_line(line: &str, key: &str, value: &str) -> Expect {
    let value = value.to_string();
    match key {
        "action" => Expect::Action(value),
        "reason" => Expect::Reason(value),
        "stdout" => Expect::Stdout(value),
        "background" => Expect::Background(value),
        _ => {
            if let Some(path) = key.strip_prefix("get ") {
                Expect::Get(path.to_string(), value)
            } else if let Some(path) = key.strip_prefix("await ") {
                Expect::Await(path.to_string(), value)
            } else {
                panic!("bad expect line `{line}`")
            }
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn steps(case: &Path) -> Vec<PathBuf> {
    let mut steps: Vec<PathBuf> = std::fs::read_dir(case)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    steps.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    steps
}

/// Put a step's files (all but `expect`) into the program directory the way an editor saves:
/// each file is written to a temporary name and renamed into place, so `velt dev` never sees
/// it half-written, and files new to the directory come first, so a module exists before the
/// files that import it change. Written now, they get the current time as their mtime (a copy
/// would keep the source's, and two versions with the same length and checkout time would look
/// unchanged).
fn apply(step: &Path, dir: &Path) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(step)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.file_name().is_some_and(|n| n != "expect"))
        .collect();
    files.sort_by_key(|path| (dir.join(path.file_name().unwrap()).exists(), path.clone()));
    for path in files {
        save(
            &dir.join(path.file_name().unwrap()),
            std::fs::read(&path).unwrap(),
        );
    }
}

/// Write `path` like an editor: to a temporary name next to it (not a `.vlt` file), then renamed
/// into place, so a watcher never sees it half-written.
fn save(path: &Path, contents: impl AsRef<[u8]>) {
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap());
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

/// What a case keeps between expectations.
struct Run<'d> {
    dev: &'d Dev,
    mode: Mode,
    probe: Option<Probe>,
    /// When this step's action happened.
    acted: Option<Instant>,
    background: HashMap<String, Background>,
}

fn run_case(case: &Path, mode: Mode) -> Result<(), String> {
    let dir = TestDir::new();
    let mut dev: Option<Dev> = None;
    let mut probe = None;
    let mut background = HashMap::new();
    for (i, step) in steps(case).iter().enumerate() {
        let ctx = format!("step {}", i + 1);
        let text = std::fs::read_to_string(step.join("expect")).unwrap();
        let expect = parse_expect(&text, mode);
        let mark = dev.as_ref().map(Dev::mark);
        if i > 0 {
            // Let the previous version's file times settle apart from the new ones.
            std::thread::sleep(Duration::from_millis(50));
        }
        apply(step, dir.path());
        let dev = dev.get_or_insert_with(|| Dev::start(dir.path(), mode.flags()));
        let mut run = Run {
            dev,
            mode,
            probe: probe.take(),
            acted: None,
            background: std::mem::take(&mut background),
        };
        for e in &expect {
            run.check(mark.unwrap_or_default(), e)
                .map_err(|m| format!("{ctx}: {m}"))?;
        }
        probe = run.probe;
        background = run.background;
    }
    if let Some(probe) = probe {
        let refused = probe.stop();
        if refused > 0 {
            return Err(format!("{refused} connections were refused during reloads"));
        }
    }
    Ok(())
}

impl Run<'_> {
    fn check(&mut self, mark: Mark, e: &Expect) -> Result<(), String> {
        match e {
            Expect::Action(action) => {
                self.acted = Some(self.action(mark, action)?);
                Ok(())
            }
            Expect::Reason(reason) if self.mode == Mode::Jit => self
                .dev
                .wait_stderr(mark, &format!("velt dev: restarted ({reason})"))
                .map(drop),
            Expect::Reason(_) => Ok(()),
            Expect::Stdout(line) => self.dev.wait_stdout(mark, line),
            Expect::Get(path, body) => self.get(path, body),
            Expect::Background(path) => {
                let port = self.dev.port()?;
                self.background
                    .insert(path.clone(), Background::start(port, path));
                Ok(())
            }
            Expect::Await(path, body) => self.finish(path, body),
        }
    }

    /// Wait for the stderr line of `action`; when it appeared.
    fn action(&self, mark: Mark, action: &str) -> Result<Instant, String> {
        let needles: &[&str] = match (action, self.mode) {
            ("start", _) => &["velt dev: started"],
            ("swap", Mode::Jit) => &["velt dev: hot-swapped"],
            ("swap", Mode::Exe) | ("restart", _) => &["velt dev: reloaded", "velt dev: restarted"],
            ("keep", _) => &["(the previous version keeps running)"],
            ("exit", _) => &["velt dev: program exited"],
            (other, _) => return Err(format!("unknown action `{other}`")),
        };
        self.dev.wait_stderr_any(mark, needles)
    }

    fn get(&mut self, path: &str, body: &str) -> Result<(), String> {
        let port = self.dev.port()?;
        if self.probe.is_none() {
            self.probe = Some(Probe::start(port));
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match get(port, path) {
                Ok(got) if got == *body => return Ok(()),
                other if Instant::now() > deadline => {
                    let log = self.dev.transcript();
                    return Err(format!(
                        "GET {path}: want `{body}`, got {other:?}:
{log}"
                    ));
                }
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// The background GET of `path` answered `body`, and only after this step's action.
    fn finish(&mut self, path: &str, body: &str) -> Result<(), String> {
        let request = self
            .background
            .remove(path)
            .ok_or_else(|| format!("no background GET {path}"))?;
        let (arrived, got) = request.finish();
        if got.as_deref() != Ok(body) {
            return Err(format!("background GET {path}: want `{body}`, got {got:?}"));
        }
        match self.acted {
            Some(acted) if arrived > acted => Ok(()),
            _ => Err(format!(
                "background GET {path} finished before the reload: it was not in flight"
            )),
        }
    }
}

#[test]
fn reload_goldens() {
    reload_support::build_runtime();
    let filter = std::env::var("VELT_RELOAD").unwrap_or_default();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(root().join("tests/reload"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.to_string_lossy().contains(&filter))
        .collect();
    cases.sort();
    let mut failures = vec![];
    for case in &cases {
        for mode in [Mode::Jit, Mode::Exe] {
            if let Err(msg) = run_case(case, mode) {
                let name = case.file_name().unwrap().to_string_lossy();
                failures.push(format!("{name} {mode:?}: {msg}"));
            }
        }
    }
    println!("reload: {} cases, {} failures", cases.len(), failures.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// SIGTERM to the supervisor alone (a process manager, `docker stop`) stops the program the
/// way a reload does and waits for it before `velt dev` exits; the port is closed afterwards.
#[cfg(unix)]
#[test]
fn sigterm_stops_the_program() {
    let dir = TestDir::new();
    apply(&root().join("tests/reload/hello_server/1"), dir.path());
    let mut dev = Dev::start(dir.path(), &[]);
    let started = dev.wait_stderr(Mark::default(), "velt dev: started");
    started.unwrap_or_else(|e| panic!("{e}"));
    let port = dev.port().unwrap_or_else(|e| panic!("{e}"));
    assert!(get(port, "/").is_ok());
    dev.signal(libc::SIGTERM);
    assert_eq!(
        dev.wait_exit(Duration::from_secs(30)),
        Some(128 + libc::SIGTERM)
    );
    assert!(get(port, "/").is_err(), "the program outlived velt dev");
}

/// After a restart, the next reload starts a spare host beside the running one (Linux): a
/// failed build's diagnostics appear once, and the spare is killed when the running host fails
/// the build or swaps the change in.
#[cfg(target_os = "linux")]
#[test]
fn spare_host_is_discarded() {
    let dir = TestDir::new();
    let case = root().join("tests/reload/hello_server/1");
    apply(&case, dir.path());
    let main = std::fs::read_to_string(case.join("main.vlt")).unwrap();
    let write = |text: String| save(&dir.path().join("main.vlt"), text);
    let dev = Dev::start(dir.path(), &[]);
    let ok = |r: Result<Instant, String>| r.unwrap_or_else(|e| panic!("{e}"));
    ok(dev.wait_stderr(Mark::default(), "velt dev: started"));
    // `main` changed: a restart, so the next reload gets a spare.
    let mark = dev.mark();
    write(main.replace("1000000000", "1000000001"));
    ok(dev.wait_stderr(mark, "velt dev: restarted"));
    let one_program = |what: &str| {
        // A hang guard: the spare is gone at once, but a loaded machine may take its time.
        let deadline = Instant::now() + Duration::from_secs(60);
        while dev.children().len() != 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            dev.children().len(),
            1,
            "{what}: the spare host is still running"
        );
    };
    one_program("after the restart");
    // A failed build: one diagnostic, and the spare is gone.
    let mark = dev.mark();
    write(
        main.replace("1000000000", "1000000001")
            .replace("new Response(", "Response.txt("),
    );
    ok(dev.wait_stderr(mark, "velt dev: build failed"));
    std::thread::sleep(Duration::from_millis(300));
    let errors = dev.stderr_since(mark);
    let count = errors
        .iter()
        .filter(|l| l.contains("is not a static method"))
        .count();
    assert_eq!(count, 1, "{errors:?}");
    one_program("after the failed build");
    // A body edit swaps (still with a spare: the last finished reload restarted); the spare
    // is killed.
    let mark = dev.mark();
    write(main.replace("1000000000", "1000000001").replace("v1", "v2"));
    ok(dev.wait_stderr(mark, "velt dev: hot-swapped"));
    one_program("after the swap");
    let port = dev.port().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(get(port, "/"), Ok("v2".to_string()));
}

/// A host that crashes while it takes a change is reported as such, not as an ordinary reload,
/// and a new host runs the change (#880: a crash during a hot swap printed `reloaded`).
#[test]
fn crash_during_reload_is_reported() {
    let dir = TestDir::new();
    let case = root().join("tests/reload/hello_server/1");
    apply(&case, dir.path());
    let main = std::fs::read_to_string(case.join("main.vlt")).unwrap();
    let dev = Dev::start_with_env(dir.path(), &[], &[("VELT_DEV_CRASH_ON_RELOAD", "1")]);
    let ok = |r: Result<Instant, String>| r.unwrap_or_else(|e| panic!("{e}"));
    ok(dev.wait_stderr(Mark::default(), "velt dev: started"));
    let mark = dev.mark();
    save(&dir.path().join("main.vlt"), main.replace("v1", "v2"));
    let crashed = "velt dev: the running program crashed during the reload (";
    ok(dev.wait_stderr(mark, crashed));
    let lines = dev.stderr_since(mark);
    let line = lines.iter().find(|l| l.contains(crashed)).unwrap();
    assert!(line.contains("); restarted it in "), "{line}");
    assert!(
        !lines.iter().any(|l| l.contains("velt dev: reloaded")),
        "{lines:?}"
    );
    let port = dev.port().unwrap_or_else(|e| panic!("{e}"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while get(port, "/").ok().as_deref() != Some("v2") {
        assert!(Instant::now() < deadline, "the new host does not serve v2");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A host started with `VELT_DEV_QUIET` (a spare) prints nothing about its build; without it
/// the same failed build prints its diagnostics.
#[test]
fn quiet_host_prints_no_diagnostics() {
    let dir = TestDir::new();
    let main = std::fs::read_to_string(root().join("tests/reload/hello_server/1/main.vlt"));
    let bad = main.unwrap().replace("new Response(", "Response.txt(");
    save(&dir.path().join("main.vlt"), bad);
    let host = |quiet: bool| {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(["dev", "--host", "main.vlt"])
            .current_dir(dir.path());
        if quiet {
            cmd.env("VELT_DEV_QUIET", "1");
        }
        let out = cmd.output().expect("run velt dev --host");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, stderr) = host(false);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("is not a static method"), "{stderr}");
    assert_eq!(host(true), (Some(1), String::new()));
}

/// A second SIGTERM exits at once, and takes the program along: the program is killed when
/// `velt dev` exits, instead of draining or running on, orphaned. Checked by how the program
/// ended, not by how soon: the test process becomes the subreaper of its descendants, so the
/// orphaned program becomes its child and its exit status can be read.
///
/// The program is stopped (SIGSTOP) before the first SIGTERM, so it cannot drain and exit by
/// itself; only a SIGKILL ends it. `velt dev`'s graceful stop sends that too, but only after
/// its stop grace (set to a minute here, so a loaded machine cannot delay the second SIGTERM
/// past it), and then reaps the program itself: the program ends as a SIGKILLed zombie handed
/// on to the test process only through the second-interrupt path.
#[cfg(target_os = "linux")]
#[test]
fn second_interrupt_kills_the_program() {
    let dir = TestDir::new();
    apply(&root().join("tests/reload/hello_server/1"), dir.path());
    // Before `Dev`: dropped after it, so it reaps the program `Dev`'s drop kills.
    let mut reaper = Subreaper::become_one();
    let mut dev = Dev::start_with_env(dir.path(), &[], &[("VELT_DEV_STOP_GRACE_MS", "60000")]);
    let started = dev.wait_stderr(Mark::default(), "velt dev: started");
    started.unwrap_or_else(|e| panic!("{e}"));
    let port = dev.port().unwrap_or_else(|e| panic!("{e}"));
    assert!(get(port, "/").is_ok());
    let programs = dev.children();
    assert_eq!(programs.len(), 1, "{programs:?}");
    let program = programs[0] as libc::pid_t;
    reaper.program = Some(program);
    // SAFETY: plain syscall on a child of `velt dev`, which has not reaped it (it is running).
    let rc = unsafe { libc::kill(program, libc::SIGSTOP) };
    assert_eq!(rc, 0, "SIGSTOP: {}", std::io::Error::last_os_error());
    dev.signal(libc::SIGTERM);
    // A second signal sent before the first is delivered would merge with it.
    let deadline = Instant::now() + Duration::from_secs(60);
    while dev.signal_pending(libc::SIGTERM) {
        assert!(
            Instant::now() < deadline,
            "the first SIGTERM was not delivered"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    dev.signal(libc::SIGTERM);
    assert_eq!(
        dev.wait_exit(Duration::from_secs(60)),
        Some(128 + libc::SIGTERM)
    );
    // The program was killed before `velt dev` exited: a zombie handed on to us. A hang guard
    // only: a program left stopped would wait forever.
    let mut status = 0;
    let deadline = Instant::now() + Duration::from_secs(60);
    let reaped = loop {
        // SAFETY: plain syscall; `status` is writable.
        match unsafe { libc::waitpid(program, &mut status, libc::WNOHANG) } {
            0 => {}
            pid => break pid,
        }
        assert!(Instant::now() < deadline, "the program outlived velt dev");
        std::thread::sleep(Duration::from_millis(5));
    };
    let error = std::io::Error::last_os_error();
    reaper.program = None;
    assert_eq!(
        reaped, program,
        "waitpid: {error} (ECHILD: `velt dev` reaped the program itself, after its stop grace)"
    );
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "the program was not killed: wait status {status:#x}"
    );
    assert!(get(port, "/").is_err(), "the port is still open");
}

/// Makes the test process the subreaper of its orphaned descendants (Linux) until dropped.
/// Drop kills and reaps `program` if it was not reaped yet, so a failed test leaves no stopped
/// process behind.
#[cfg(target_os = "linux")]
struct Subreaper {
    program: Option<libc::pid_t>,
}

#[cfg(target_os = "linux")]
impl Subreaper {
    fn become_one() -> Subreaper {
        // SAFETY: plain syscall; it only changes who reaps this process's orphaned
        // descendants. Other tests' orphans then end as zombies of the test process, which
        // they count as gone.
        let rc = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        assert_eq!(rc, 0, "prctl: {}", std::io::Error::last_os_error());
        Subreaper { program: None }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Subreaper {
    fn drop(&mut self) {
        if let Some(program) = self.program {
            // SAFETY: plain syscalls. `program` is not reaped yet (we would have cleared it),
            // so its pid is not reused; if it is `velt dev`'s child still, waitpid fails.
            unsafe {
                libc::kill(program, libc::SIGKILL);
                libc::waitpid(program, std::ptr::null_mut(), 0);
            }
        }
        // SAFETY: plain syscall.
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0, 0, 0, 0) };
    }
}

/// How the benchmark edits `hello_server` for edit number `n`.
#[derive(Clone, Copy, Debug)]
enum BenchEdit {
    /// The response text only: a hot swap in JIT mode.
    Body,
    /// The response text and `main` (a restart in JIT mode too).
    BodyAndMain,
}

/// Save → first response from the new version (median of 10 edits of hello_server): JIT hot
/// swap, JIT restart, and `--exe` restart.
#[test]
#[ignore]
fn bench_save_to_first_response() {
    reload_support::build_runtime();
    let runs = [
        (Mode::Jit, BenchEdit::Body),
        (Mode::Jit, BenchEdit::BodyAndMain),
        (Mode::Exe, BenchEdit::Body),
    ];
    for (mode, edit) in runs {
        let times = bench(mode, edit);
        println!(
            "save → first response, {mode:?} {edit:?}: median {:.1} ms, min {:.1} ms, max {:.1} ms",
            times[times.len() / 2].as_secs_f64() * 1e3,
            times[0].as_secs_f64() * 1e3,
            times[times.len() - 1].as_secs_f64() * 1e3
        );
    }
}

/// Sorted save → first response times of 10 edits.
fn bench(mode: Mode, edit: BenchEdit) -> Vec<Duration> {
    let case = root().join("tests/reload/hello_server/1");
    let dir = TestDir::new();
    apply(&case, dir.path());
    let source = std::fs::read_to_string(dir.path().join("main.vlt")).unwrap();
    let dev = Dev::start(dir.path(), mode.flags());
    dev.wait_stderr(Mark::default(), "velt dev: started")
        .unwrap();
    let port = dev.port().unwrap();
    let mut times = vec![];
    for n in 2..12 {
        std::thread::sleep(Duration::from_millis(100));
        let body = format!("v{n}");
        let mut text = source.replace("\"v1\"", &format!("\"{body}\""));
        if let BenchEdit::BodyAndMain = edit {
            text = text.replace("sleep(1000000000)", &format!("sleep(100000000{n})"));
        }
        let saved = Instant::now();
        save(&dir.path().join("main.vlt"), text);
        while get(port, "/").ok().as_deref() != Some(body.as_str()) {
            std::thread::sleep(Duration::from_millis(1));
        }
        times.push(saved.elapsed());
    }
    times.sort();
    times
}
