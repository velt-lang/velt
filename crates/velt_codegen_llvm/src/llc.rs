//! Compiling IR for the WebAssembly targets with LLVM's `opt` + `llc`, or else the clang of the
//! pinned zig.
//!
//! The system clang is not used for WebAssembly: Apple's clang is built without the WebAssembly
//! backend, while rustup's `llvm-tools` component (`rustup component add llvm-tools`) ships
//! `opt`/`llc` with it on every host. Search order for the directory holding both tools:
//! `$VELT_LLVM_BIN`, the active Rust toolchain's `lib/rustlib/<host>/bin`, `PATH`, then Homebrew's
//! LLVM. A candidate counts only if `llc --version` reports LLVM 16+ with a `wasm32` target. The
//! result is cached per process.
//!
//! Without them (a release installed on a machine with no Rust, #2587), the IR goes to `zig cc`:
//! the zig the toolchain downloads on first use for C headers and sources, whose clang has every
//! LLVM backend. The command registers how to get it ([`set_zig`]), as this crate doesn't
//! download anything. `$VELT_LLVM_BIN` stays final: when set, zig is not tried.

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

/// Gives the path of the pinned zig, downloading it on first use: see [`set_zig`].
pub type ZigSource = fn() -> Result<PathBuf, String>;

static ZIG: OnceLock<ZigSource> = OnceLock::new();

/// Registers how to get the pinned zig, for compiling WebAssembly when no `llc` and `opt` are
/// found. Called once by the command, before code generation; later calls are ignored.
pub fn set_zig(source: ZigSource) {
    let _ = ZIG.set(source);
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
        let explicit = std::env::var_os("VELT_LLVM_BIN").is_some_and(|d| !d.is_empty());
        if let (false, Some(source)) = (explicit, ZIG.get()) {
            let zig = source().map_err(|e| {
                format!(
                    "WebAssembly targets need LLVM's `llc` and `opt` (`rustup component add \
                     llvm-tools`) or zig, which could not be had: {e}"
                )
            })?;
            return compile_with_zig(&zig, ir, target, optimize);
        }
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

/// zig's name for a WebAssembly target.
fn zig_target(triple: &str) -> &'static str {
    if triple.contains("wasi") {
        "wasm32-wasi"
    } else {
        "wasm32-freestanding"
    }
}

/// Compile `ir` to a WebAssembly object file with `zig cc` (clang): optimized as `opt` + `llc`
/// would be, for the same generic CPU `llc` defaults to. zig reads and writes files only (it
/// takes neither `-x ir` from stdin nor `-o -`), so both go through a directory of their own.
pub(crate) fn compile_with_zig(
    zig: &Path,
    ir: &str,
    target: &Target,
    optimize: bool,
) -> CodegenResult<Vec<u8>> {
    let dir = ZigDir::new()?;
    let (input, output) = (dir.0.join("unit.ll"), dir.0.join("unit.o"));
    std::fs::write(&input, ir).map_err(|e| format!("codegen: cannot write the IR for zig: {e}"))?;
    let mut cmd = Command::new(zig);
    cmd.args(["cc", "-target", zig_target(&target.triple), "-mcpu=generic"])
        // The IR names `wasm32-unknown-unknown`, which zig spells with one more `-unknown`.
        .arg("-Wno-override-module")
        .arg(if optimize {
            crate::clang::opt_flag()
        } else {
            "-O0"
        })
        .arg("-c")
        .arg(&input)
        .arg("-o")
        .arg(&output);
    run_piped(cmd, b"", "zig cc")?;
    std::fs::read(&output).map_err(|e| format!("codegen: zig cc wrote no object: {e}"))
}

/// A directory of one zig compile's own, removed when dropped: units compile in parallel.
struct ZigDir(PathBuf);

impl ZigDir {
    fn new() -> CodegenResult<ZigDir> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("velt-wasm-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("codegen: cannot make a directory for zig: {e}"))?;
        Ok(ZigDir(dir))
    }
}

impl Drop for ZigDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
