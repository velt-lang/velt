//! The `velt doctor` smoke test: compile a hello-world in a scratch directory with the real
//! pipeline (loader, std, codegen, linker, runtime library) and run it — once as a debug build and,
//! when clang is available, once as a release build through the LLVM backend. On macOS it also
//! reports how long the new executable took to start: the first-launch check macOS runs on every
//! new program can dominate `velt run`, and a terminal listed under Developer Tools skips it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{Check, Status};
use crate::backend::Backend;
use crate::driver::{self, Artifact, BuildError, BuildOptions, Session};

const PROGRAM: &str = "function main() {\n  console.log(\"hello from velt doctor\");\n}\n";
const EXPECTED: &str = "hello from velt doctor\n";

/// The debug smoke test, plus the release one if `clang_found`.
pub fn run(clang_found: bool) -> Vec<Check> {
    let dir = match Scratch::new() {
        Ok(dir) => dir,
        Err(msg) => {
            let hint = "make the system temp directory writable (or point TMP/TMPDIR elsewhere)";
            return vec![Check::bad("hello world", Status::Fail, msg, hint)];
        }
    };
    let (debug, first_launch) = smoke(&dir.path, "hello (debug)", false);
    let mut checks = vec![debug];
    if cfg!(target_os = "macos") {
        checks.extend(first_launch.map(first_launch_check));
    }
    if clang_found {
        checks.push(smoke(&dir.path, "hello (release)", true).0);
    } else {
        checks.push(Check::bad(
            "hello (release)",
            Status::Warn,
            "skipped: needs clang for the LLVM backend",
            "see the clang check above",
        ));
    }
    checks
}

/// First launches slower than this get the Developer Tools hint.
const SLOW_FIRST_LAUNCH: Duration = Duration::from_millis(100);

/// The check, and how long the fresh executable took to run.
fn smoke(dir: &Path, name: &'static str, release: bool) -> (Check, Option<Duration>) {
    let hint = "fix the failing checks above; if they all pass, this is a bug — please report it \
                with the output of `velt doctor`";
    match build_and_run(dir, release) {
        Ok(took) => {
            let how = if release {
                "built with LLVM and ran"
            } else {
                "built with Cranelift and ran"
            };
            (Check::ok(name, how), Some(took))
        }
        Err(msg) => (Check::bad(name, Status::Fail, msg, hint), None),
    }
}

/// macOS: report the first launch of a new executable; warn when it is slow.
fn first_launch_check(took: Duration) -> Check {
    let ms = took.as_millis();
    if took < SLOW_FIRST_LAUNCH {
        return Check::ok("first launch", format!("{ms} ms"));
    }
    Check::bad(
        "first launch",
        Status::Warn,
        format!("a new program took {ms} ms to start (macOS checks every new executable)"),
        "add your terminal app under System Settings → Privacy & Security → Developer Tools\n\
         no Developer Tools entry? run `sudo spctl developer-mode enable-terminal` (docs/tooling/platforms.md)",
    )
}

/// Build hello world and run it once; how long the run took.
fn build_and_run(dir: &Path, release: bool) -> Result<Duration, String> {
    let source = dir.join("hello.vlt");
    std::fs::write(&source, PROGRAM)
        .map_err(|e| format!("cannot write `{}`: {e}", source.display()))?;
    let stem = if release {
        "hello-release"
    } else {
        "hello-debug"
    };
    let exe = dir.join(if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    });
    let opts = BuildOptions {
        input: source,
        output: Some(exe),
        release,
        backend: Some(if release {
            Backend::Llvm
        } else {
            Backend::Cranelift
        }),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    let exe = match driver::build(&mut sess, &opts) {
        Ok(Artifact::Executable(exe)) => exe,
        Ok(other) => return Err(format!("ICE: built {other:?} instead of an executable")),
        Err(BuildError::Diagnostics) => {
            return Err(format!("compile error:\n{}", sess.render_diagnostics()))
        }
        Err(BuildError::Failed(msg) | BuildError::Ice(msg)) => return Err(msg),
    };
    let start = Instant::now();
    let out = Command::new(&exe)
        .output()
        .map_err(|e| format!("cannot run `{}`: {e}", exe.display()))?;
    let took = start.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || stdout != EXPECTED {
        return Err(format!(
            "the program misbehaved ({}): stdout {stdout:?}, stderr {:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(took)
}

/// A scratch directory under the system temp dir, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Scratch, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!("velt-doctor-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("cannot create `{}`: {e}", path.display()))?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_first_launch_warns_with_the_developer_tools_hint() {
        let fast = first_launch_check(Duration::from_millis(20));
        assert_eq!((fast.status, fast.detail.as_str()), (Status::Ok, "20 ms"));
        let slow = first_launch_check(Duration::from_millis(250));
        assert_eq!(slow.status, Status::Warn);
        assert!(slow.hint.unwrap().contains("Developer Tools"));
    }
}
