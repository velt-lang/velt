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

mod reload_support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reload_support::{get, Background, Dev, Mark, Probe};

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

/// Copy a step's files (all but `expect`) into the program directory, stamped with the current
/// time: a copy keeps the source's mtime, and two versions of a file with the same length and
/// checkout time would look unchanged to `velt dev`.
fn apply(step: &Path, dir: &Path) {
    for entry in std::fs::read_dir(step).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n != "expect") {
            let dst = dir.join(path.file_name().unwrap());
            std::fs::copy(&path, &dst).unwrap();
            let file = std::fs::File::options().write(true).open(&dst).unwrap();
            file.set_modified(std::time::SystemTime::now()).unwrap();
        }
    }
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
    let dir = tempfile::tempdir().unwrap();
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
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match get(port, path) {
                Ok(got) if got == *body => return Ok(()),
                other if Instant::now() > deadline => {
                    return Err(format!("GET {path}: want `{body}`, got {other:?}"))
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
    let dir = tempfile::tempdir().unwrap();
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
        std::fs::write(dir.path().join("main.vlt"), text).unwrap();
        while get(port, "/").ok().as_deref() != Some(body.as_str()) {
            std::thread::sleep(Duration::from_millis(1));
        }
        times.push(saved.elapsed());
    }
    times.sort();
    times
}
