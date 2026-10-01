//! WebAssembly linking with `wasm-ld`: program object + `libvelt_rt_wasm.a` → `.wasm` module.
//!
//! - `wasm32-wasip1`: a WASI command module. wasi-libc's `crt1-command.o` provides `_start`
//!   (which calls the runtime's `__main_void`) and `libc.a` what Rust's `std` needs. Both come
//!   from `$VELT_WASI_SYSROOT` (a directory holding them, e.g. wasi-sdk's
//!   `share/wasi-sysroot/lib/wasm32-wasip1`) or the self-contained copy rustup installs with
//!   `rustup target add wasm32-wasip1`.
//! - `wasm32-unknown-unknown`: a browser module without entry point; it exports `velt_start`
//!   and `memory`, and imports its host services from the `velt` module (the JS glue in
//!   `editors/web/velt_web.js`).
//!
//! The linker is `$VELT_LINKER`, else the `rust-lld` of the active Rust toolchain (`-flavor
//! wasm`), else `wasm-ld` on `PATH`. rust-lld comes first because it matches the wasi-libc that
//! rustup installs: a newer libc needs a linker at least as new (an older system `wasm-ld` fails
//! with undefined symbols such as `__wasm_first_page_end`). The stack is 8 MiB like a native main
//! thread (wasm-ld's default is 64 KiB, too small for recursive programs).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{run_linker, LinkRequest};

/// File name of the WebAssembly runtime library.
pub const RUNTIME_LIB_NAME: &str = "libvelt_rt_wasm.a";

const STACK_SIZE: &str = "stack-size=8388608";

/// The two supported WebAssembly flavors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmFlavor {
    /// `wasm32-wasip1` (`wasm32-wasi`): WASI command modules.
    Wasi,
    /// `wasm32-unknown-unknown`: browser modules driven by the JS glue.
    Browser,
}

impl WasmFlavor {
    /// Classify a target triple; `None` for non-WebAssembly targets.
    pub fn from_triple(target: &str) -> Option<WasmFlavor> {
        match target.trim() {
            "wasm32-wasip1" | "wasm32-wasi" | "wasm32-unknown-wasip1" => Some(WasmFlavor::Wasi),
            "wasm32-unknown-unknown" => Some(WasmFlavor::Browser),
            _ => None,
        }
    }

    /// The Rust target triple the runtime library is built for.
    pub fn rust_triple(self) -> &'static str {
        match self {
            WasmFlavor::Wasi => "wasm32-wasip1",
            WasmFlavor::Browser => "wasm32-unknown-unknown",
        }
    }
}

/// Link a WebAssembly module (see the module docs).
pub fn link(req: &LinkRequest, flavor: WasmFlavor) -> Result<(), String> {
    if !req.runtime_lib.is_file() {
        return Err(format!(
            "runtime library not found: {}",
            req.runtime_lib.display()
        ));
    }
    if let Some(dir) = req.output.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let (mut cmd, flavor_args) = find_wasm_ld()?;
    cmd.args(flavor_args);
    cmd.args(args(req, flavor)?);
    run_linker(cmd).map_err(explain_missing_symbols)
}

fn args(req: &LinkRequest, flavor: WasmFlavor) -> Result<Vec<OsString>, String> {
    let mut args: Vec<OsString> = vec!["-o".into(), req.output.into()];
    let libc = match flavor {
        WasmFlavor::Wasi => {
            let sysroot = wasi_libc_dir()?;
            args.push(sysroot.join("crt1-command.o").into());
            Some(sysroot)
        }
        WasmFlavor::Browser => {
            args.extend(["--no-entry", "--export=velt_start"].map(OsString::from));
            None
        }
    };
    args.extend(req.objects.iter().map(OsString::from));
    args.push(req.runtime_lib.into());
    if let Some(dir) = libc {
        args.push(dir.join("libc.a").into());
    }
    // A signature mismatch between compiled code and the runtime would link to a trapping
    // stub; make it an error instead.
    args.extend(["-z", STACK_SIZE, "--fatal-warnings"].map(OsString::from));
    if req.release {
        args.push("--strip-debug".into());
    }
    Ok(args)
}

/// The linker command plus the arguments that select its WebAssembly flavor.
pub fn find_wasm_ld() -> Result<(Command, Vec<&'static str>), String> {
    if let Some(cmd) = crate::linker_override() {
        return Ok((cmd, vec![]));
    }
    let rust_lld = rust_host_bin()
        .map(|d| d.join(format!("rust-lld{}", std::env::consts::EXE_SUFFIX)))
        .filter(|p| p.is_file());
    let wasm_ld = || {
        let name = PathBuf::from(format!("wasm-ld{}", std::env::consts::EXE_SUFFIX));
        runs(&name).then_some(name)
    };
    match choose_linker(rust_lld, wasm_ld) {
        Some(WasmLinker::RustLld(p)) => Ok((Command::new(p), vec!["-flavor", "wasm"])),
        Some(WasmLinker::WasmLd(p)) => Ok((Command::new(p), vec![])),
        None => Err(
            "no WebAssembly linker found: install Rust (its rust-lld links wasm), put \
             LLVM's `wasm-ld` on PATH, or set $VELT_LINKER"
                .into(),
        ),
    }
}

/// A WebAssembly linker found on this machine.
#[derive(Debug, PartialEq, Eq)]
enum WasmLinker {
    /// The Rust toolchain's `rust-lld` (run with `-flavor wasm`).
    RustLld(PathBuf),
    /// A `wasm-ld` on `PATH`.
    WasmLd(PathBuf),
}

/// The Rust toolchain's linker wins over a `wasm-ld` on `PATH` (see the module docs); `wasm_ld`
/// is only probed when there is no rust-lld.
fn choose_linker(
    rust_lld: Option<PathBuf>,
    wasm_ld: impl FnOnce() -> Option<PathBuf>,
) -> Option<WasmLinker> {
    rust_lld
        .map(WasmLinker::RustLld)
        .or_else(|| wasm_ld().map(WasmLinker::WasmLd))
}

fn runs(program: &Path) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `rustc --print sysroot`.
fn rust_sysroot() -> Option<PathBuf> {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(text.trim())).filter(|p| p.is_dir())
}

/// `<sysroot>/lib/rustlib/<host>/bin` (rust-lld, and llvm-tools when installed).
fn rust_host_bin() -> Option<PathBuf> {
    let out = Command::new("rustc").arg("-vV").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let host = text.lines().find_map(|l| l.strip_prefix("host: "))?.trim();
    Some(
        rust_sysroot()?
            .join("lib")
            .join("rustlib")
            .join(host)
            .join("bin"),
    )
}

/// Directory with wasi-libc's `crt1-command.o` and `libc.a`.
pub fn wasi_libc_dir() -> Result<PathBuf, String> {
    let has_libc = |d: &Path| d.join("crt1-command.o").is_file() && d.join("libc.a").is_file();
    if let Some(dir) = std::env::var_os("VELT_WASI_SYSROOT").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        return if has_libc(&dir) {
            Ok(dir)
        } else {
            Err(format!(
                "$VELT_WASI_SYSROOT (`{}`) has no crt1-command.o + libc.a",
                dir.display()
            ))
        };
    }
    rust_sysroot()
        .map(|s| s.join("lib/rustlib/wasm32-wasip1/lib/self-contained"))
        .filter(|d| has_libc(d))
        .ok_or_else(|| {
            "wasi-libc not found: run `rustup target add wasm32-wasip1`, or set \
             $VELT_WASI_SYSROOT to a directory with crt1-command.o and libc.a"
                .into()
        })
}

/// Runtime library candidates for `flavor` next to a toolchain directory `dir` (the directory
/// of `velt`, or cargo's `target/<profile>`): cargo's cross-build layout
/// `<dir>/../<triple>/<profile>/` and the installed `<prefix>/lib/<triple>/`.
pub fn runtime_lib_candidates(dir: &Path, flavor: WasmFlavor) -> Vec<PathBuf> {
    let triple = flavor.rust_triple();
    let mut out = vec![];
    for d in [Some(dir), dir.parent()].into_iter().flatten() {
        if let (Some(parent), Some(profile)) = (d.parent(), d.file_name()) {
            out.push(parent.join(triple).join(profile).join(RUNTIME_LIB_NAME));
        }
    }
    if let Some(prefix) = dir.parent() {
        out.push(prefix.join("lib").join(triple).join(RUNTIME_LIB_NAME));
    }
    out
}

/// Programs using the parts of the runtime WebAssembly lacks fail with undefined `velt_rt_*`
/// symbols, and a declaration that disagrees with the runtime with a signature mismatch; say
/// why.
fn explain_missing_symbols(msg: String) -> String {
    if msg.contains("undefined symbol: velt_rt_") {
        format!(
            "{msg}\nnote: WebAssembly programs cannot use TCP, HTTP or child processes (the \
             wasm runtime has no sockets or processes)"
        )
    } else if msg.contains("signature mismatch") {
        format!(
            "{msg}\nnote: a runtime function is declared with a different signature than \
             libvelt_rt_wasm.a defines (a compiler/runtime bug; please report it)"
        )
    } else {
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavors() {
        assert_eq!(
            WasmFlavor::from_triple("wasm32-wasip1"),
            Some(WasmFlavor::Wasi)
        );
        assert_eq!(
            WasmFlavor::from_triple("wasm32-wasi"),
            Some(WasmFlavor::Wasi)
        );
        assert_eq!(
            WasmFlavor::from_triple("wasm32-unknown-unknown"),
            Some(WasmFlavor::Browser)
        );
        assert_eq!(WasmFlavor::from_triple("aarch64-apple-darwin"), None);
    }

    #[test]
    fn browser_args_export_the_entry() {
        let req = LinkRequest {
            target: "wasm32-unknown-unknown",
            objects: &[PathBuf::from("p.o")],
            runtime_lib: Path::new("rt.a"),
            output: Path::new("p.wasm"),
            release: true,
        };
        let args: Vec<String> = args(&req, WasmFlavor::Browser)
            .unwrap()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-o",
                "p.wasm",
                "--no-entry",
                "--export=velt_start",
                "p.o",
                "rt.a",
                "-z",
                STACK_SIZE,
                "--fatal-warnings",
                "--strip-debug"
            ]
        );
    }

    #[test]
    fn rust_lld_is_preferred_over_wasm_ld_on_path() {
        let lld = PathBuf::from("/rust/bin/rust-lld");
        let on_path = || Some(PathBuf::from("wasm-ld"));
        assert_eq!(
            choose_linker(Some(lld.clone()), || panic!("wasm-ld probed")),
            Some(WasmLinker::RustLld(lld))
        );
        assert_eq!(
            choose_linker(None, on_path),
            Some(WasmLinker::WasmLd("wasm-ld".into()))
        );
        assert_eq!(choose_linker(None, || None), None);
    }

    #[test]
    fn runtime_lib_search_paths() {
        let dir = Path::new("/w/target/debug");
        let c = runtime_lib_candidates(dir, WasmFlavor::Wasi);
        assert_eq!(
            c[0],
            Path::new("/w/target/wasm32-wasip1/debug/libvelt_rt_wasm.a")
        );
        // Test binaries live in target/<profile>/deps.
        let c = runtime_lib_candidates(&dir.join("deps"), WasmFlavor::Browser);
        assert!(c.contains(&PathBuf::from(
            "/w/target/wasm32-unknown-unknown/debug/libvelt_rt_wasm.a"
        )));
        assert!(
            explain_missing_symbols("undefined symbol: velt_rt_tcp_listen".into())
                .contains("cannot use TCP")
        );
    }
}
