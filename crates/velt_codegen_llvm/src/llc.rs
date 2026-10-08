//! Compiling IR for the WebAssembly targets with LLVM's `opt` + `llc`.
//!
//! clang is not used for WebAssembly: Apple's clang is built without the WebAssembly backend,
//! while rustup's `llvm-tools` component (`rustup component add llvm-tools`) ships `opt`/`llc`
//! with it on every host. Search order for the directory holding both tools: `$VELT_LLVM_BIN`,
//! the active Rust toolchain's `lib/rustlib/<host>/bin`, `PATH`, then Homebrew's LLVM. A
//! candidate counts only if `llc --version` reports LLVM 16+ with a `wasm32` target. The result
//! is cached per process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::clang::run_piped;
use crate::target::Target;
use crate::CodegenResult;

/// Oldest LLVM whose IR parser accepts what the backend emits (see `clang`).
const MIN_LLVM_MAJOR: u32 = 16;

/// `opt` and `llc` from one LLVM installation.
#[derive(Clone, Debug)]
pub struct LlvmTools {
    /// The IR optimizer.
    pub opt: PathBuf,
    /// The static compiler (IR → object file).
    pub llc: PathBuf,
}

/// The LLVM tools used for WebAssembly targets, if any were found.
pub fn find_wasm_tools() -> Option<LlvmTools> {
    static FOUND: OnceLock<Option<LlvmTools>> = OnceLock::new();
    FOUND.get_or_init(search).clone()
}

fn search() -> Option<LlvmTools> {
    let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = std::env::var_os("VELT_LLVM_BIN").filter(|d| !d.is_empty()) {
        // An explicit choice is final.
        return usable(
            &Path::new(&dir).join(exe("llc")),
            &Path::new(&dir).join(exe("opt")),
        );
    }
    let mut dirs: Vec<PathBuf> = rust_toolchain_bin().into_iter().collect();
    dirs.push(PathBuf::new()); // PATH
    if !cfg!(windows) {
        dirs.push("/opt/homebrew/opt/llvm/bin".into());
        dirs.push("/usr/local/opt/llvm/bin".into());
    }
    dirs.iter()
        .find_map(|d| usable(&d.join(exe("llc")), &d.join(exe("opt"))))
}

/// `<sysroot>/lib/rustlib/<host>/bin` of the `rustc` on `PATH` (where `llvm-tools` installs).
fn rust_toolchain_bin() -> Option<PathBuf> {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = String::from_utf8(out.stdout).ok()?;
    let host = Command::new("rustc").arg("-vV").output().ok()?;
    let host = String::from_utf8(host.stdout).ok()?;
    let triple = host.lines().find_map(|l| l.strip_prefix("host: "))?;
    Some(
        Path::new(sysroot.trim())
            .join("lib")
            .join("rustlib")
            .join(triple.trim())
            .join("bin"),
    )
}

fn usable(llc: &Path, opt: &Path) -> Option<LlvmTools> {
    let out = Command::new(llc).arg("--version").output().ok()?;
    let banner = String::from_utf8_lossy(&out.stdout);
    let ok = out.status.success()
        && llvm_major(&banner).is_some_and(|m| m >= MIN_LLVM_MAJOR)
        && banner.contains("wasm32");
    let opt_runs = Command::new(opt)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    (ok && opt_runs).then(|| LlvmTools {
        opt: opt.to_path_buf(),
        llc: llc.to_path_buf(),
    })
}

/// The major version in `llc --version` output (`LLVM version 22.1.8-rust-1.98.1-stable`).
fn llvm_major(banner: &str) -> Option<u32> {
    let rest = banner.split("LLVM version ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Compile `ir` to a WebAssembly object file: `opt -O3` (or `$VELT_LLVM_OPT`; release) then `llc`.
pub(crate) fn compile(ir: &str, target: &Target, optimize: bool) -> CodegenResult<Vec<u8>> {
    let Some(tools) = find_wasm_tools() else {
        bail!(
            "WebAssembly targets need LLVM's `llc` and `opt` with the WebAssembly backend: run \
             `rustup component add llvm-tools`, or set $VELT_LLVM_BIN to an LLVM {MIN_LLVM_MAJOR}+ \
             bin directory"
        )
    };
    let mut input = std::borrow::Cow::Borrowed(ir.as_bytes());
    if optimize {
        let mut cmd = Command::new(&tools.opt);
        cmd.args([
            crate::clang::opt_flag(),
            &format!("-mtriple={}", target.triple),
            "-",
            "-o",
            "-",
        ]);
        input = run_piped(cmd, &input, "opt")?.into();
    }
    let mut cmd = Command::new(&tools.llc);
    cmd.arg(if optimize { "-O3" } else { "-O0" })
        .args(["-filetype=obj", &format!("-mtriple={}", target.triple)])
        .args(["-", "-o", "-"]);
    run_piped(cmd, &input, "llc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_banner() {
        let rust = "LLVM (http://llvm.org/):\n  LLVM version 22.1.8-rust-1.98.1-stable\n";
        assert_eq!(llvm_major(rust), Some(22));
        assert_eq!(llvm_major("Homebrew LLVM version 19.1.7\n"), Some(19));
        assert_eq!(llvm_major("gcc"), None);
    }
}
