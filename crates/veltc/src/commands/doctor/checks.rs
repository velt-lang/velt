//! Environment checks for `velt doctor`: version, runtime libraries (static, shared), std, system
//! linker, WebAssembly linker, clang and the vpm home directory. Each returns a [`Check`] with a
//! fix hint when something is missing.

use std::path::Path;

use super::{Check, Status};
use crate::loader;

/// Label of the clang check (the smoke test runs a release build only when it passed).
pub const CLANG: &str = "clang";

/// All environment checks, in report order.
pub fn environment() -> Vec<Check> {
    let host = velt_codegen_cl::host_triple();
    vec![
        Check::ok("velt", crate::commands::version::version_line()),
        runtime_lib(&host),
        shared_runtime_lib(&host),
        std_lib(),
        linker(&host),
        wasm_linker(),
        clang(),
        velt_home(),
    ]
}

fn runtime_lib(host: &str) -> Check {
    match velt_link::find_runtime_lib(host) {
        Ok(path) => {
            let profile = match velt_link::runtime_lib_is_debug(&path) {
                Some(true) => " (debug build: `--release` programs run slowly with it)",
                Some(false) => " (release build)",
                None => "",
            };
            Check::ok("runtime lib", format!("{}{profile}", path.display()))
        }
        Err(msg) => Check::bad(
            "runtime lib",
            Status::Fail,
            msg,
            format!(
                "an installed toolchain keeps it in <prefix>/lib/{} next to <prefix>/bin/velt; \
                 reinstall, or set $VELT_RT_LIB",
                velt_link::runtime_lib_name(host)
            ),
        ),
    }
}

/// The shared runtime is optional: without it debug builds link the static one, only slower.
fn shared_runtime_lib(host: &str) -> Check {
    const LABEL: &str = "shared runtime";
    match velt_link::find_shared_runtime_lib(host) {
        Some(path) => Check::ok(LABEL, format!("{} (debug builds)", path.display())),
        None if std::env::var_os("VELT_RT_LINK").is_some_and(|v| v == "static")
            || std::env::var_os("VELT_RT_LIB").is_some_and(|v| !v.is_empty()) =>
        {
            Check::ok(LABEL, "not used ($VELT_RT_LINK=static or $VELT_RT_LIB is set)")
        }
        None => Check::bad(
            LABEL,
            Status::Warn,
            "not found: debug builds link the static runtime (slower)",
            format!(
                "an installed toolchain keeps it in <prefix>/lib/{} (`cargo build -p velt_rt_shared` \
                 in a checkout)",
                velt_link::shared_runtime_lib_name(
                    velt_link::TargetOs::from_triple(host).unwrap_or_else(velt_link::TargetOs::host)
                )
            ),
        ),
    }
}

fn std_lib() -> Check {
    let hint = "an installed toolchain keeps it in <prefix>/std next to <prefix>/bin; reinstall, \
                or set $VELT_STD to the std directory";
    match loader::std_root() {
        Some(root) if !root.is_dir() => Check::bad(
            "std",
            Status::Fail,
            format!("`{}` is not a directory", root.display()),
            hint,
        ),
        Some(root) if loader::prelude_files(&root).is_empty() => Check::bad(
            "std",
            Status::Warn,
            format!("{} (no prelude/*.vlt found)", root.display()),
            "the prelude (Array/Map methods, assert...) will be unavailable; reinstall the std sources",
        ),
        Some(root) => Check::ok("std", root.display().to_string()),
        None => Check::bad("std", Status::Fail, "standard library not found", hint),
    }
}

fn linker(host: &str) -> Check {
    match velt_link::find_linker(host) {
        Ok(path) => Check::ok("linker", path.display().to_string()),
        Err(msg) => Check::bad("linker", Status::Fail, "no usable system linker", msg),
    }
}

/// WebAssembly is optional: without a linker only `--target wasm32-*` builds fail.
fn wasm_linker() -> Check {
    const LABEL: &str = "wasm linker";
    match velt_link::find_linker("wasm32-wasip1") {
        Ok(path) => Check::ok(LABEL, path.display().to_string()),
        Err(msg) => Check::bad(
            LABEL,
            Status::Warn,
            format!("{msg} (optional: only `--target wasm32-*` builds need it)"),
            "install Rust (`rustup`): its rust-lld links WebAssembly",
        ),
    }
}

fn clang() -> Check {
    match velt_codegen_llvm::find_clang() {
        Some(path) => Check::ok(CLANG, path.display().to_string()),
        None => Check::bad(
            CLANG,
            Status::Warn,
            format!(
                "{} (optional: `--release` falls back to the Cranelift backend)",
                velt_codegen_llvm::rejected_clang().unwrap_or_else(|| "not found".into())
            ),
            if cfg!(windows) {
                "install LLVM (`winget install LLVM.LLVM`) or set $VELT_CLANG"
            } else if cfg!(target_os = "macos") {
                "install LLVM (`brew install llvm`) or set $VELT_CLANG"
            } else {
                // Distribution default `clang` packages may be too old (Ubuntu 22.04: 14).
                "install clang 16 or newer (e.g. `sudo apt install clang-18`) or set $VELT_CLANG"
            },
        ),
    }
}

fn velt_home() -> Check {
    let hint = "set $VELT_HOME (and/or $VELT_REGISTRY) to a writable directory";
    let loc = match vpm::Locations::from_env() {
        Ok(loc) => loc,
        Err(msg) => return Check::bad("velt home", Status::Fail, msg, hint),
    };
    for dir in [&loc.registry, &loc.cache] {
        if let Err(msg) = probe_writable(dir) {
            return Check::bad("velt home", Status::Fail, msg, hint);
        }
    }
    Check::ok(
        "velt home",
        format!(
            "registry {}, cache {}",
            loc.registry.display(),
            loc.cache.display()
        ),
    )
}

/// Create `dir` if needed, then write and delete a probe file in it.
fn probe_writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    let probe = dir.join(format!(".vlt-doctor-{}", std::process::id()));
    std::fs::write(&probe, b"ok")
        .map_err(|e| format!("`{}` is not writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        probe_writable(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "probe file left behind"
        );
    }
}
