//! Locating `clang` and compiling textual IR with it.
//!
//! Search order: `$VELT_CLANG`, `clang` on `PATH`, then the standard install locations
//! (`C:\Program Files\LLVM\bin` on Windows; Homebrew / `/usr/bin` / versioned `clang-NN` on
//! unix). A candidate counts only if `clang --version` runs and reports LLVM 16 or newer: the
//! emitted IR uses opaque `ptr` types (LLVM 15+) and `memory(...)` attributes (LLVM 16+), and
//! Linux distributions often ship an older default `clang` (Ubuntu 20.04: 10, 22.04: 14) next
//! to a newer `clang-NN`. The result is cached per process.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::target::Target;
use crate::CodegenResult;

/// Oldest LLVM major version whose IR parser accepts what the backend emits.
const MIN_LLVM_MAJOR: u32 = 16;
/// Apple clang numbers releases on its own scheme; Apple clang 15 is based on LLVM 16.
const MIN_APPLE_CLANG_MAJOR: u32 = 15;

/// Whether a working clang was found, i.e. whether [`crate::emit_object`] can work.
pub fn available() -> bool {
    find_clang().is_some()
}

/// The clang executable used by the backend, if any.
pub fn find_clang() -> Option<PathBuf> {
    search_result().found.clone()
}

/// When no usable clang was found but a too-old one was, why it was rejected (for diagnostics).
pub fn rejected_clang() -> Option<String> {
    let search = search_result();
    if search.found.is_some() {
        return None;
    }
    search.too_old.clone()
}

/// Outcome of the (cached) clang search.
struct Search {
    found: Option<PathBuf>,
    /// The first candidate that runs but is too old, described.
    too_old: Option<String>,
}

fn search_result() -> &'static Search {
    static SEARCH: OnceLock<Search> = OnceLock::new();
    SEARCH.get_or_init(search)
}

fn search() -> Search {
    let explicit = std::env::var_os("VELT_CLANG").filter(|p| !p.is_empty());
    // An explicit choice is final: silently using another clang would be surprising.
    let list = match explicit {
        Some(p) => vec![PathBuf::from(p)],
        None => candidates(),
    };
    let mut too_old = None;
    for clang in list {
        match probe(&clang) {
            Probe::Usable => {
                return Search {
                    found: Some(clang),
                    too_old: None,
                }
            }
            Probe::TooOld(version) if too_old.is_none() => {
                too_old = Some(format!(
                    "`{}` is {version}, but the LLVM backend needs clang {MIN_LLVM_MAJOR} or newer",
                    clang.display()
                ));
            }
            Probe::TooOld(_) | Probe::Missing => {}
        }
    }
    Search {
        found: None,
        too_old,
    }
}

fn candidates() -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "clang.exe" } else { "clang" };
    let mut list = vec![PathBuf::from(exe)];
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(dir) = std::env::var_os(var) {
                list.push(Path::new(&dir).join("LLVM").join("bin").join(exe));
            }
        }
        list.push(PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe"));
    } else {
        for dir in [
            "/opt/homebrew/opt/llvm/bin",
            "/usr/local/opt/llvm/bin",
            "/usr/local/bin",
            "/usr/bin",
        ] {
            list.push(Path::new(dir).join("clang"));
        }
        for version in (MIN_LLVM_MAJOR..=30).rev() {
            list.push(PathBuf::from(format!("clang-{version}")));
        }
    }
    list
}

/// Whether a clang candidate can compile the backend's IR.
enum Probe {
    Usable,
    /// Runs, but too old; carries the version for the diagnostic (e.g. `clang 14.0.0`).
    TooOld(String),
    Missing,
}

fn probe(clang: &Path) -> Probe {
    let Ok(out) = Command::new(clang).arg("--version").output() else {
        return Probe::Missing;
    };
    if !out.status.success() {
        return Probe::Missing;
    }
    match parse_version(&String::from_utf8_lossy(&out.stdout)) {
        Some(v) if v.major < v.min_major() => Probe::TooOld(v.describe()),
        // An unrecognized version banner gets the benefit of the doubt.
        _ => Probe::Usable,
    }
}

/// A clang version as printed by `clang --version`.
#[derive(Debug, PartialEq, Eq)]
struct ClangVersion {
    apple: bool,
    major: u32,
    full: String,
}

impl ClangVersion {
    fn min_major(&self) -> u32 {
        if self.apple {
            MIN_APPLE_CLANG_MAJOR
        } else {
            MIN_LLVM_MAJOR
        }
    }

    fn describe(&self) -> String {
        let vendor = if self.apple { "Apple clang" } else { "clang" };
        format!("{vendor} {}", self.full)
    }
}

/// Parse the first line of `clang --version`: `[Vendor ]clang version X.Y.Z[ (...)]`.
fn parse_version(text: &str) -> Option<ClangVersion> {
    let line = text.lines().next()?;
    let (prefix, rest) = line.split_once("clang version ")?;
    let full = rest.split_whitespace().next()?.to_string();
    let major = full.split('.').next()?.parse().ok()?;
    Some(ClangVersion {
        apple: prefix.trim_end().ends_with("Apple"),
        major,
        full,
    })
}

/// Compile `ir` to an object file for `target` and return its bytes.
pub(crate) fn compile(ir: &str, target: &Target, optimize: bool) -> CodegenResult<Vec<u8>> {
    let Some(clang) = find_clang() else {
        let what = rejected_clang().unwrap_or_else(|| "the LLVM backend needs clang".into());
        bail!(
            "{what}; set $VELT_CLANG, add clang {MIN_LLVM_MAJOR}+ to PATH, or install LLVM (e.g. \
             `winget install LLVM.LLVM`, `sudo apt install clang-18`, `brew install llvm`); or \
             use `--backend cranelift`"
        )
    };
    let mut cmd = Command::new(&clang);
    cmd.arg(if optimize { opt_flag() } else { "-O0" })
        .args(["-c", "-x", "ir", "-Wno-override-module"])
        .arg(format!("--target={}", target.triple))
        .args(target.deployment_target_arg());
    if target.pic() {
        cmd.arg("-fPIC");
    }
    cmd.args(["-", "-o", "-"]);
    run_piped(cmd, ir.as_bytes(), "clang")
}

/// Run an LLVM tool that reads its input from stdin and writes its output to stdout (`-` for
/// both), and return the output. Nothing goes through files: a scratch file just written by a
/// tool could be unreadable for a moment on Windows (another process, a virus scanner, still
/// holding it), and a scratch path can collide with one a crashed process left behind.
pub(crate) fn run_piped(mut cmd: Command, input: &[u8], name: &str) -> CodegenResult<Vec<u8>> {
    let program = cmd.get_program().to_owned();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "codegen: cannot run `{}`: {e}",
                Path::new(&program).display()
            )
        })?;
    let mut stdin = child.stdin.take().expect("ICE: stdin is piped");
    // Feed the input from another thread while the output is read here: a tool may write
    // before it has read everything, and both pipes have small buffers.
    let (out, fed) = std::thread::scope(|scope| {
        let feeder = scope.spawn(move || stdin.write_all(input));
        let out = child.wait_with_output();
        let fed = feeder.join().expect("ICE: the stdin feeder panicked");
        out.map(|out| (out, fed))
    })
    .map_err(|e| {
        format!(
            "codegen: cannot run `{}`: {e}",
            Path::new(&program).display()
        )
    })?;
    if !out.status.success() {
        bail!(
            "codegen: {name} failed on the generated IR (this is a compiler bug):\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A tool that failed may stop reading: then its error above is reported, not the broken
    // pipe. One that succeeded without reading all of its input compiled only part of it.
    fed.map_err(|e| format!("codegen: cannot write the IR to {name}: {e}"))?;
    Ok(out.stdout)
}

/// clang's optimization flag for release builds: `-O3`, or `$VELT_LLVM_OPT` (`1`, `2`, `3`, `s`
/// or `z`, with or without the `-O`) to trade code speed for compile time.
pub(crate) fn opt_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let level = std::env::var("VELT_LLVM_OPT").unwrap_or_default();
        parse_opt_level(&level).unwrap_or("-O3")
    })
}

fn parse_opt_level(level: &str) -> Option<&'static str> {
    Some(match level.trim().trim_start_matches("-O") {
        "1" => "-O1",
        "2" => "-O2",
        "3" => "-O3",
        "s" => "-Os",
        "z" => "-Oz",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usable(banner: &str) -> Option<bool> {
        parse_version(banner).map(|v| v.major >= v.min_major())
    }

    #[test]
    fn optimization_levels() {
        assert_eq!(parse_opt_level("2"), Some("-O2"));
        assert_eq!(parse_opt_level("-O2"), Some("-O2"));
        assert_eq!(parse_opt_level(" s "), Some("-Os"));
        assert_eq!(parse_opt_level(""), None);
        assert_eq!(parse_opt_level("fast"), None);
    }

    #[test]
    fn version_banners() {
        let win = "clang version 22.1.8 (https://github.com/llvm/llvm-project ca7933e)\nTarget: x";
        assert_eq!(usable(win), Some(true));
        assert_eq!(
            usable("Ubuntu clang version 18.1.8 (11~20.04.2)"),
            Some(true)
        );
        assert_eq!(usable("Ubuntu clang version 16.0.6 (++2023)"), Some(true));
        // Ubuntu 22.04's default clang: no `memory(...)` attributes yet.
        assert_eq!(
            usable("Ubuntu clang version 14.0.0-1ubuntu1.1"),
            Some(false)
        );
        assert_eq!(usable("clang version 10.0.0-4ubuntu1"), Some(false));
        // Apple clang 15 is LLVM 16; Apple clang 14 is LLVM 15.
        assert_eq!(
            usable("Apple clang version 15.0.0 (clang-1500.3.9.4)"),
            Some(true)
        );
        assert_eq!(
            usable("Apple clang version 14.0.3 (clang-1403.0.22.14.1)"),
            Some(false)
        );
        assert_eq!(
            parse_version("Ubuntu clang version 14.0.0-1ubuntu1.1")
                .unwrap()
                .describe(),
            "clang 14.0.0-1ubuntu1.1"
        );
        assert_eq!(usable("gcc (GCC) 13.2.0"), None);
    }
}
