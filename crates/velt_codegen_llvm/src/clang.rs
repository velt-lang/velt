//! Locating `clang` and compiling textual IR with it.
//!
//! Search order: `$VELT_CLANG`, `clang` on `PATH`, then the standard install locations
//! (`C:\Program Files\LLVM\bin` on Windows; Homebrew / `/usr/bin` / versioned `clang-NN` on
//! unix). A candidate counts only if `clang --version` runs and reports LLVM 16 or newer: the
//! emitted IR uses opaque `ptr` types (LLVM 15+) and `memory(...)` attributes (LLVM 16+), and
//! Linux distributions often ship an older default `clang` (Ubuntu 20.04: 10, 22.04: 14) next
//! to a newer `clang-NN`. The result is cached per process.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

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

/// Compile `ir` to an object file for `target` and return its bytes; from `cache` when this clang
/// compiled the same IR with the same flags before (on this machine, in any build), else storing
/// it there.
pub(crate) fn compile_cached(
    ir: &str,
    target: &Target,
    optimize: bool,
    cache: Option<&dyn velt_codegen_cl::CodeCache>,
) -> CodegenResult<(Vec<u8>, bool)> {
    let Some(clang) = find_clang() else {
        let what = rejected_clang().unwrap_or_else(|| "the LLVM backend needs clang".into());
        bail!(
            "{what}; set $VELT_CLANG, add clang {MIN_LLVM_MAJOR}+ to PATH, or install LLVM (e.g. \
             `winget install LLVM.LLVM`, `sudo apt install clang-18`, `brew install llvm`); or \
             use `--backend cranelift`"
        )
    };
    let mut args: Vec<String> = vec![if optimize { opt_flag() } else { "-O0" }.into()];
    args.extend(["-c", "-x", "ir", "-Wno-override-module"].map(String::from));
    args.push(format!("--target={}", target.triple));
    args.extend(target.cpu_arg());
    args.extend(target.deployment_target_arg().map(String::from));
    if target.pic() {
        args.push("-fPIC".into());
    }
    if target.outline_atomics() {
        args.push("-moutline-atomics".into());
    }
    args.extend(["-", "-o", "-"].map(String::from));
    // The key: this clang (path, size and time of its executable), its arguments (`-march=native`
    // means this machine's CPU: the cache is the machine's), and the IR.
    let key = cache.map(|_| {
        let meta = std::fs::metadata(&clang).ok();
        let stamp = meta.map(|m| (m.len(), m.modified().ok()));
        let id = format!(
            "clang {clang:?} {stamp:?} {args:?} {}",
            velt_codegen_cl::host_triple()
        );
        let mut key = id.into_bytes();
        key.push(0);
        key.extend_from_slice(ir.as_bytes());
        key
    });
    if let (Some(cache), Some(key)) = (cache, &key) {
        if let Some(obj) = cache.get(key) {
            return Ok((obj, true));
        }
    }
    let mut cmd = Command::new(&clang);
    cmd.args(&args);
    let obj = run_piped(cmd, ir.as_bytes(), "clang")
        .map_err(|e| unknown_cpu(e, target.cpu.as_deref()))?;
    if let (Some(cache), Some(key)) = (cache, &key) {
        cache.put(key, &obj);
    }
    Ok((obj, false))
}

/// The x86 features clang lets code for `-march=<cpu>` on `triple` use, as a mask over
/// `velt_codegen_cl::X86_FEATURES`: from the macros it defines (`clang -dM -E`), once per CPU
/// and process. Without a clang, Cranelift's idea of the CPU (`velt build --emit llvm` needs no
/// clang; everything that compiles the IR does).
pub(crate) fn x86_features(triple: &str, cpu: &str) -> CodegenResult<u32> {
    static FEATURES: OnceLock<Mutex<HashMap<(String, String), u32>>> = OnceLock::new();
    let features = FEATURES.get_or_init(Default::default);
    let key = (triple.to_string(), cpu.to_string());
    if let Some(mask) = features.lock().ok().and_then(|f| f.get(&key).copied()) {
        return Ok(mask);
    }
    let Some(clang) = find_clang() else {
        return velt_codegen_cl::required_features(triple, Some(cpu));
    };
    let out = Command::new(&clang)
        .args(["-dM", "-E", "-x", "c"])
        .arg(format!("--target={triple}"))
        .arg(format!("-march={cpu}"))
        .arg("-")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("codegen: cannot run `{}`: {e}", clang.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(unknown_cpu(
            format!("codegen: `clang -march={cpu}` failed:\n{err}"),
            Some(cpu),
        ));
    }
    let mask = velt_codegen_cl::x86_features_from_macros(&String::from_utf8_lossy(&out.stdout));
    if let Ok(mut f) = features.lock() {
        f.insert(key, mask);
    }
    Ok(mask)
}

/// `err` from clang, as the user's error when it is that clang does not know the CPU `cpu` (a
/// name newer than the clang in use: the names are checked before, `cpu_names.rs`).
fn unknown_cpu(err: String, cpu: Option<&str>) -> String {
    match cpu {
        Some(cpu) if err.contains("unknown target CPU") => {
            let clang = find_clang()
                .map(|c| c.display().to_string())
                .unwrap_or_default();
            format!(
                "the clang in use (`{clang}`) does not know CPU `{cpu}`: use a newer clang \
                 ($VELT_CLANG) or name another CPU"
            )
        }
        _ => err,
    }
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
pub fn opt_flag() -> &'static str {
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
    fn named_cpus_have_the_features_clang_gives_them() {
        if find_clang().is_none() {
            return;
        }
        let names = |mask: u32| {
            (0..velt_codegen_cl::X86_FEATURES.len())
                .filter(|b| mask & 1 << b != 0)
                .map(|b| velt_codegen_cl::X86_FEATURES[b].0)
                .collect::<Vec<_>>()
        };
        let linux = "x86_64-unknown-linux-gnu";
        // Cranelift's presets of these have no AVX2 (nor MOVBE and F16C): clang's do.
        for cpu in ["alderlake", "znver4", "haswell"] {
            let has = names(x86_features(linux, cpu).unwrap());
            for f in ["AVX", "AVX2", "MOVBE", "F16C", "POPCNT"] {
                assert!(has.contains(&f), "{cpu}: {f} in {has:?}");
            }
        }
        let skx = names(x86_features(linux, "skylake-avx512").unwrap());
        assert!(
            skx.contains(&"AVX512BW") && skx.contains(&"AVX512CD"),
            "{skx:?}"
        );
        assert!(!names(x86_features(linux, "core2").unwrap()).contains(&"SSE4.1"));
        let err = "clang: error: unknown target CPU 'apple-m9'".to_string();
        let msg = unknown_cpu(err.clone(), Some("apple-m9"));
        assert!(msg.contains("does not know CPU `apple-m9`"), "{msg}");
        assert_eq!(unknown_cpu(err.clone(), None), err);
        assert_eq!(unknown_cpu("other".into(), Some("znver4")), "other");
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
