//! One differential check: run a program under Node (the oracle) and under every `velt` build
//! mode, then classify the outcome as a [`Verdict`].
//!
//! Node's behavior is the expected behavior. Compile time and run time are budgeted separately so
//! a slow build is never mistaken for a looping program.

use crate::exec::{self, Output, Status};
use crate::tsify;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A `velt build` configuration a program is compiled and run under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Unoptimized Cranelift build.
    Debug,
    /// `--release` with the default backend (LLVM when clang is available).
    Release,
    /// `--release --backend cranelift` (velt_opt + optimizing Cranelift).
    ReleaseCranelift,
    /// The program after `velt fmt`, built in debug: formatting must not change behavior.
    /// Opt-in (`--modes …,fmt`).
    Formatted,
}

impl Mode {
    /// The default modes, in the order they are checked.
    pub const ALL: [Mode; 3] = [Mode::Debug, Mode::Release, Mode::ReleaseCranelift];

    /// Every mode `--modes` accepts.
    pub const KNOWN: [Mode; 4] = [
        Mode::Debug,
        Mode::Release,
        Mode::ReleaseCranelift,
        Mode::Formatted,
    ];

    /// Short name used in reports and signatures.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Debug => "debug",
            Mode::Release => "release",
            Mode::ReleaseCranelift => "release-cl",
            Mode::Formatted => "fmt",
        }
    }

    fn flags(self) -> &'static [&'static str] {
        match self {
            Mode::Debug | Mode::Formatted => &[],
            Mode::Release => &["--release"],
            Mode::ReleaseCranelift => &["--release", "--backend", "cranelift"],
        }
    }
}

/// Where the tools are and how long they may take.
#[derive(Clone, Debug)]
pub struct Config {
    /// The `velt` binary under test.
    pub velt: PathBuf,
    /// The `node` binary (≥ 22.7, for `--experimental-transform-types`).
    pub node: PathBuf,
    /// Budget for one `velt build`.
    pub build_timeout: Duration,
    /// Budget for one program run (Node or native).
    pub run_timeout: Duration,
    /// Build modes to compare against the oracle.
    pub modes: Vec<Mode>,
    /// Whose behavior is expected.
    pub oracle: Oracle,
}

/// Which behavior is the expected one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Oracle {
    /// Node runs the TypeScript twin (programs of the shared subset).
    Node,
    /// The debug build (unoptimized Cranelift); the release modes must match it. For programs
    /// outside the JS-compatible subset: wrapping integers, casts, integer division.
    Debug,
}

/// What a run printed and how it ended — the observable behavior being compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Behavior {
    /// Captured stdout.
    pub stdout: String,
    /// Exit status.
    pub status: Status,
}

/// The classified outcome of one check.
#[derive(Clone, Debug)]
pub enum Verdict {
    /// Every mode behaved exactly like the oracle.
    Agree,
    /// Node could not run the program (syntax/type/reference error): it is outside the shared
    /// subset, so nothing can be concluded.
    NodeRejected(String),
    /// Node ran it but `velt` refused to compile it (first diagnostic line).
    VeltRejected(String),
    /// The compiler itself crashed (panic, ICE, signal) in `mode`.
    CompilerCrash { mode: Mode, message: String },
    /// A build ran but behaved differently from the oracle.
    Mismatch {
        mode: Mode,
        expected: Behavior,
        actual: Behavior,
        stderr: String,
    },
    /// The oracle ran out of time (the program is probably too slow or loops): inconclusive.
    OracleTimeout,
}

impl Verdict {
    /// A short, location-free key: two verdicts with the same signature are presumed to be the
    /// same bug (used to dedupe and to keep the shrinker on the original failure).
    pub fn signature(&self) -> String {
        match self {
            Verdict::Agree => "agree".into(),
            Verdict::NodeRejected(_) => "node-rejected".into(),
            Verdict::OracleTimeout => "oracle-timeout".into(),
            Verdict::VeltRejected(msg) => format!("velt-rejected: {}", strip_location(msg)),
            Verdict::CompilerCrash { mode, message } => {
                format!("crash[{}]: {}", mode.name(), strip_location(message))
            }
            Verdict::Mismatch {
                mode,
                expected,
                actual,
                ..
            } => {
                format!(
                    "mismatch[{}]: {}",
                    mode.name(),
                    mismatch_kind(expected, actual)
                )
            }
        }
    }

    /// True for verdicts that point at a compiler bug (as opposed to an unusable program).
    pub fn is_bug(&self) -> bool {
        matches!(
            self,
            Verdict::CompilerCrash { .. } | Verdict::Mismatch { .. }
        )
    }
}

/// Runs the program at `src` under the oracle and every configured mode. `scratch` must be a
/// directory private to this call (parallel checks each get their own).
pub fn check(cfg: &Config, src: &Path, scratch: &Path) -> Result<Verdict, String> {
    let text =
        fs::read_to_string(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    check_source(cfg, &text, scratch)
}

/// Like [`check`], for in-memory source (generated programs, shrinker candidates).
pub fn check_source(cfg: &Config, text: &str, scratch: &Path) -> Result<Verdict, String> {
    fs::create_dir_all(scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let prog = scratch.join("prog.vlt");
    fs::write(&prog, text).map_err(|e| format!("cannot write program: {e}"))?;
    let oracle = match cfg.oracle {
        Oracle::Node => node_behavior(cfg, text, scratch)?,
        Oracle::Debug => build_and_run(cfg, Mode::Debug, &prog, scratch)?.map(|(b, _)| b),
    };
    let expected = match oracle {
        Ok(b) if b.status == Status::Timeout => return Ok(Verdict::OracleTimeout),
        Ok(b) => b,
        Err(verdict) => return Ok(verdict),
    };
    let modes = cfg
        .modes
        .iter()
        .filter(|m| cfg.oracle == Oracle::Node || **m != Mode::Debug);
    for &mode in modes {
        match build_and_run(cfg, mode, &prog, scratch)? {
            Err(verdict) => return Ok(verdict),
            Ok((actual, stderr)) if actual != expected => {
                return Ok(Verdict::Mismatch {
                    mode,
                    expected,
                    actual,
                    stderr,
                })
            }
            Ok(_) => {}
        }
    }
    Ok(Verdict::Agree)
}

/// Node's behavior on the TypeScript twin, or the verdict that makes it unusable as an oracle.
fn node_behavior(
    cfg: &Config,
    text: &str,
    scratch: &Path,
) -> Result<Result<Behavior, Verdict>, String> {
    let ts = scratch.join("prog.ts");
    fs::write(&ts, tsify::to_typescript(text)).map_err(|e| format!("cannot write twin: {e}"))?;
    let node = exec::run(
        Command::new(&cfg.node)
            // Transform (not only strip) types: TS `enum`s need code.
            .arg("--experimental-transform-types")
            .arg("--no-warnings")
            .arg("--stack-size=4000")
            .arg(&ts),
        scratch,
        cfg.run_timeout,
    )?;
    if let Some(reason) = node_rejected(&node) {
        return Ok(Err(Verdict::NodeRejected(reason)));
    }
    Ok(Ok(Behavior {
        stdout: node.stdout,
        status: node.status,
    }))
}

/// Builds `prog` in `mode` and runs it: its behavior and stderr, or the build's verdict.
fn build_and_run(
    cfg: &Config,
    mode: Mode,
    prog: &Path,
    scratch: &Path,
) -> Result<Result<(Behavior, String), Verdict>, String> {
    let prog = &match mode {
        Mode::Formatted => match formatted(cfg, prog, scratch)? {
            Ok(p) => p,
            Err(verdict) => return Ok(Err(verdict)),
        },
        _ => prog.to_path_buf(),
    };
    let exe = scratch.join(format!(
        "prog-{}{}",
        mode.name(),
        std::env::consts::EXE_SUFFIX
    ));
    let build = exec::run(
        Command::new(&cfg.vlt)
            .arg("build")
            .arg(prog)
            .args(mode.flags())
            .arg("-o")
            .arg(&exe),
        scratch,
        cfg.build_timeout,
    )?;
    if let Some(message) = compiler_crash(&build) {
        return Ok(Err(Verdict::CompilerCrash { mode, message }));
    }
    if build.status != Status::Exit(0) {
        return Ok(Err(Verdict::VeltRejected(first_error(&build.stderr))));
    }
    let run = exec::run(&mut Command::new(&exe), scratch, cfg.run_timeout)?;
    let behavior = Behavior {
        stdout: run.stdout,
        status: run.status,
    };
    Ok(Ok((behavior, run.stderr)))
}

/// A `velt fmt`-formatted copy of `prog`; a formatter crash or rejection is a crash verdict
/// (the program compiles, so it parses).
fn formatted(
    cfg: &Config,
    prog: &Path,
    scratch: &Path,
) -> Result<Result<std::path::PathBuf, Verdict>, String> {
    let copy = scratch.join("prog_fmt.vlt");
    fs::copy(prog, &copy).map_err(|e| format!("cannot copy program: {e}"))?;
    let out = exec::run(
        Command::new(&cfg.vlt).arg("fmt").arg(&copy),
        scratch,
        cfg.build_timeout,
    )?;
    if out.status != Status::Exit(0) {
        let message = format!("velt fmt failed: {}", first_line(&out.stderr));
        return Ok(Err(Verdict::CompilerCrash {
            mode: Mode::Formatted,
            message,
        }));
    }
    Ok(Ok(copy))
}

/// Node failures that mean "not a program of the shared subset" rather than an uncaught throw;
/// returns the line naming the JS error.
fn node_rejected(out: &Output) -> Option<String> {
    const MARKERS: [&str; 5] = [
        "SyntaxError",
        "ReferenceError",
        "TypeError",
        "RangeError: Maximum call stack",
        "ERR_",
    ];
    if out.status == Status::Exit(0) {
        return None;
    }
    out.stderr
        .lines()
        .find(|l| MARKERS.iter().any(|m| l.contains(m)))
        .map(|l| l.trim().to_string())
}

/// The compiler must never panic or die on a signal; ICEs are reported as crashes too.
fn compiler_crash(build: &Output) -> Option<String> {
    let crashed = matches!(build.status, Status::Signal | Status::Timeout)
        || build.status == Status::Exit(101)
        || build.stderr.contains("panicked at")
        || build.stderr.contains("ICE");
    crashed.then(|| match build.status {
        Status::Timeout => "compiler timed out".to_string(),
        Status::Signal => "compiler killed by a signal".to_string(),
        Status::Exit(_) => panic_message(&build.stderr)
            .or_else(|| ice_message(&build.stderr))
            .unwrap_or_else(|| first_line(&build.stderr)),
    })
}

/// An `ICE` line; when it ends in `:` (VIR verification), plus the first detail line without
/// its function / block prefix and with numbers masked (`agg#11` → `agg#N`), so the same
/// failure dedupes across programs while different failures stay apart.
fn ice_message(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let i = lines
        .iter()
        .position(|l| l.contains("ICE") || l.contains("internal compiler error"))?;
    let head = lines[i].trim();
    if !head.ends_with(':') {
        return Some(head.to_string());
    }
    let detail = lines.get(i + 1).map_or("", |l| l.trim());
    let detail = detail.split_once(": ").map_or(detail, |(_, rest)| rest);
    let mut masked = String::new();
    for c in detail.chars() {
        match c.is_ascii_digit() {
            true if masked.ends_with('N') => {}
            true => masked.push('N'),
            false => masked.push(c),
        }
    }
    Some(format!("{head} {masked}"))
}

/// `panicked at <location>: <message>` without the thread name and id, which vary per run (the
/// shrinker must see the same signature again); the message is on the line after the location.
fn panic_message(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let i = lines.iter().position(|l| l.contains("panicked at"))?;
    let at = &lines[i][lines[i].find("panicked at")?..];
    let message = lines.get(i + 1).map_or("", |l| l.trim());
    Some(format!("{at} {message}").trim_end().to_string())
}

fn mismatch_kind(expected: &Behavior, actual: &Behavior) -> String {
    match (&expected.status, &actual.status) {
        (_, Status::Timeout) => "timeout".into(),
        (_, Status::Signal) => "signal".into(),
        (a, b) if a != b => format!("exit {:?} vs {:?}", a, b),
        _ => "stdout".into(),
    }
}

fn first_error(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.contains("error"))
        .map_or_else(|| first_line(stderr), str::to_string)
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Drops a leading `path:line:col:` so the same diagnostic dedupes across files.
fn strip_location(msg: &str) -> String {
    match msg.find(": error: ") {
        Some(i) => msg[i + 2..].to_string(),
        None => msg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_signatures_ignore_thread_ids() {
        let stderr = "\nthread 'velt-compile' (71380056) panicked at src/lower/mod.rs:88:5:\n\
                      ICE: type Error cannot be lowered\nnote: run with RUST_BACKTRACE=1\n";
        assert_eq!(
            panic_message(stderr).as_deref(),
            Some("panicked at src/lower/mod.rs:88:5: ICE: type Error cannot be lowered")
        );
    }

    #[test]
    fn ice_signatures_keep_the_detail_line() {
        let stderr = "error: internal compiler error: VIR verification failed:\n  \
                      fn#22 _V2h0 bb9: invalid projection: field 1 out of range for agg#21\n";
        assert_eq!(
            ice_message(stderr).as_deref(),
            Some(
                "error: internal compiler error: VIR verification failed: \
                 invalid projection: field N out of range for agg#N"
            )
        );
    }

    #[test]
    fn signatures_ignore_locations() {
        let a = Verdict::VeltRejected("a.vlt:3:4: error: mismatched types".into());
        let b = Verdict::VeltRejected("b.vlt:9:1: error: mismatched types".into());
        assert_eq!(a.signature(), b.signature());
        assert_eq!(a.signature(), "velt-rejected: error: mismatched types");
    }

    #[test]
    fn node_rejection_needs_a_marker() {
        let throw = Output {
            stdout: String::new(),
            stderr: "Error: boom".into(),
            status: Status::Exit(1),
        };
        assert_eq!(node_rejected(&throw), None);
        let syntax = Output {
            stderr: "at x\nSyntaxError: x".into(),
            ..throw
        };
        assert_eq!(node_rejected(&syntax).as_deref(), Some("SyntaxError: x"));
    }
}
