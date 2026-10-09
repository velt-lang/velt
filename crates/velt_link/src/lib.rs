//! Native linking: object files + velt_rt staticlib → executable, per platform.
//! The public API below is a contract (maintainer-owned).
//!
//! - The bundled linker ([`bundled`]): the toolchain's own `lld` plus a link kit for the target
//!   ([`kit`]: import libraries or stub libraries and startup objects), so no system linker,
//!   SDK or C compiler is needed. Used whenever the toolchain has both.
//! - Otherwise the system linker:
//!   - Windows (MSVC): `link.exe` is located with `cc::windows_registry::find_tool`, which reads
//!     the registry / vswhere and returns the `LIB`/`PATH` environment for the MSVC + Windows
//!     SDK libraries, so no "Developer Command Prompt" is needed.
//!   - Linux / macOS: the system C compiler driver (`cc`) is used as the linker; on Linux with
//!     `-fuse-ld=mold`/`lld` for static links when those are installed ([`fast_ld`]).
//! - Debug builds may link the runtime as a shared library instead ([`shared`]): the
//!   `runtime_lib` of a [`LinkRequest`] then names it (see [`find_shared_runtime_lib`]).
//!
//! - Native libraries of packages ([`NativeLink`], docs/internals/contracts/native_abi.md
//!   "Linking"): a prelinked object linked like the program's own objects, or a shared library
//!   (`-l` + rpath on Unix; the import library plus a copy of the DLL beside the executable on
//!   Windows).
//!
//! `$VELT_LINKER` chooses ([`bundled`]): `bundled` or `system` forces that linker; any other
//! value overrides the system linker program (the argument style stays the same).
//! Cross-linking (target OS != host OS) needs the bundled linker and a kit for the target, except
//! for the WebAssembly targets (`wasm`: `wasm-ld`, the same on every host).

use std::path::{Path, PathBuf};
use std::process::Command;

pub mod bundled;
mod fast_ld;
mod host;
pub mod kit;
mod lld;
mod native;
mod runtime_profile;
mod shared;
mod system;
pub mod wasm;

pub use host::{host_triple, same_target};
pub use native::NativeLink;
pub use runtime_profile::runtime_lib_is_debug;
pub use shared::shared_runtime_lib_name;
pub(crate) use system::{
    find_system_linker, msvc_args, msvc_linker, platform, unix_args, unix_linker,
};

/// Everything needed to link one executable.
pub struct LinkRequest<'a> {
    /// Target triple.
    pub target: &'a str,
    /// Object files to link.
    pub objects: &'a [PathBuf],
    /// Path to the velt_rt static library (`velt_rt.lib` / `libvelt_rt.a`), or to the shared
    /// runtime ([`shared_runtime_lib_name`]: linked dynamically).
    pub runtime_lib: &'a Path,
    /// Executable to produce.
    pub output: &'a Path,
    /// Link with release settings (strip debug info, etc.).
    pub release: bool,
    /// Native libraries of packages (empty for most programs).
    pub native: &'a [NativeLink],
}

/// Operating-system family a target triple links for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOs {
    /// MSVC toolchain (`link.exe`).
    Windows,
    /// Apple `cc`/`ld64`.
    MacOs,
    /// GNU/Linux `cc`.
    Linux,
}

impl TargetOs {
    /// Classify a target triple (`x86_64-pc-windows-msvc`, `aarch64-apple-darwin`, ...).
    pub fn from_triple(target: &str) -> Option<TargetOs> {
        if target.contains("windows") {
            Some(TargetOs::Windows)
        } else if target.contains("apple") || target.contains("darwin") || target.contains("macos")
        {
            Some(TargetOs::MacOs)
        } else if target.contains("linux") {
            Some(TargetOs::Linux)
        } else {
            None
        }
    }

    /// The OS this compiler runs on.
    pub fn host() -> TargetOs {
        match std::env::consts::OS {
            "windows" => TargetOs::Windows,
            "macos" => TargetOs::MacOs,
            _ => TargetOs::Linux,
        }
    }
}

/// File name of the runtime static library for `target`.
pub fn runtime_lib_name(target: &str) -> &'static str {
    if wasm::WasmFlavor::from_triple(target).is_some() {
        return wasm::RUNTIME_LIB_NAME;
    }
    let os = TargetOs::from_triple(target).unwrap_or_else(TargetOs::host);
    platform(os).runtime_lib_name
}

/// CONTRACT: produce an executable. Error string should include the linker's stderr.
pub fn link(req: &LinkRequest) -> Result<(), String> {
    if let Some(flavor) = wasm::WasmFlavor::from_triple(req.target) {
        return wasm::link(req, flavor);
    }
    let os = TargetOs::from_triple(req.target)
        .ok_or_else(|| format!("unsupported target `{}`", req.target))?;
    let choice = bundled::choose(bundled::Request::from_env(), || bundled::find(req.target))?;
    if os != TargetOs::host() && !matches!(choice, bundled::Choice::Bundled(_)) {
        return Err(format!(
            "cross-OS linking needs the bundled linker and a link kit for `{}`, and this toolchain \
             has none ({})",
            req.target,
            match &choice {
                bundled::Choice::System { why: Some(why) } => why.as_str(),
                _ => "$VELT_LINKER selects another linker",
            }
        ));
    }
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
    native::check_files(req.native)?;
    let why_not_bundled = match &choice {
        bundled::Choice::System { why } => why.clone(),
        _ => None,
    };
    match choice {
        bundled::Choice::Bundled(b) => link_with_bundled(req, &b, os),
        _ => link_with_system(req, os)
            .map_err(|e| with_bundled_reason(e, why_not_bundled.as_deref())),
    }
}

/// The system linker's link (`link.exe`, `cc`).
fn link_with_system(req: &LinkRequest, os: TargetOs) -> Result<(), String> {
    let shared = shared::is_shared(req.runtime_lib, os);
    match os {
        TargetOs::Windows => {
            let mut cmd = msvc_linker(req.target)?;
            cmd.args(msvc_args(req)?);
            run_linker(cmd)?;
            if shared {
                shared::place_dll(req.runtime_lib, req.output)?;
            }
            native::place_dlls(req.native, req.output)
        }
        TargetOs::Linux | TargetOs::MacOs => {
            let args = unix_args(req, os);
            let fast = (os == TargetOs::Linux && !shared && linker_override().is_none())
                .then(fast_ld::fuse_ld_flag)
                .flatten();
            if let Some(flag) = fast {
                let mut cmd = unix_linker();
                cmd.arg(flag).args(&args);
                if run_linker(cmd).is_ok() {
                    return Ok(());
                }
            }
            let mut cmd = unix_linker();
            cmd.args(&args);
            run_linker(cmd)
        }
    }
}

/// How the error starts when the linker program cannot be started (see [`with_bundled_reason`]).
const LINKER_NOT_RUN: &str = "failed to run linker";

/// A system link that failed because there is no system linker (`link.exe` not found, `cc`
/// not runnable), after the bundled linker was passed over for `why`: say why, since a toolchain
/// with a bundled linker should not need one. Other link errors are left as they are.
fn with_bundled_reason(err: String, why: Option<&str>) -> String {
    let no_linker = err.starts_with(system::NO_MSVC_LINKER) || err.starts_with(LINKER_NOT_RUN);
    match why {
        Some(why) if no_linker => format!("{err}\n(the bundled linker cannot be used: {why})"),
        _ => err,
    }
}

/// Link with the bundled lld and kit `b` regardless of `$VELT_LINKER` (what [`link`] does when it
/// chooses the bundled linker; tests use it with a kit of their own).
pub fn link_bundled(req: &LinkRequest, b: &bundled::Bundled) -> Result<(), String> {
    let os = TargetOs::from_triple(req.target)
        .ok_or_else(|| format!("unsupported target `{}`", req.target))?;
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
    native::check_files(req.native)?;
    link_with_bundled(req, b, os)
}

fn link_with_bundled(req: &LinkRequest, b: &bundled::Bundled, os: TargetOs) -> Result<(), String> {
    let mut cmd = b.command(lld::flavor(b.kit.kind));
    cmd.args(lld::args(req, &b.kit)?);
    run_linker(cmd)?;
    if os == TargetOs::Windows {
        if shared::is_shared(req.runtime_lib, os) {
            shared::place_dll(req.runtime_lib, req.output)?;
        }
        native::place_dlls(req.native, req.output)?;
    }
    Ok(())
}

/// Which linker [`link`] uses for a target, and why (for `velt doctor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkerReport {
    /// The bundled lld with the kit in `kit`.
    Bundled { lld: PathBuf, kit: PathBuf },
    /// The system linker `program`; `why` says why the bundled one is not used (`None` when
    /// `$VELT_LINKER` asks for the system linker or names a program).
    System {
        program: PathBuf,
        why: Option<String>,
    },
}

/// Which linker [`link`] would use for `target` (see [`LinkerReport`]), or why none is usable.
pub fn linker_report(target: &str) -> Result<LinkerReport, String> {
    if wasm::WasmFlavor::from_triple(target).is_some() {
        return find_linker(target).map(|program| LinkerReport::System { program, why: None });
    }
    match bundled::choose(bundled::Request::from_env(), || bundled::find(target))? {
        bundled::Choice::Bundled(b) => Ok(LinkerReport::Bundled {
            lld: b.lld,
            kit: b.kit.dir,
        }),
        bundled::Choice::Override | bundled::Choice::System { why: None } => {
            find_system_linker(target).map(|program| LinkerReport::System { program, why: None })
        }
        bundled::Choice::System { why: Some(why) } => find_system_linker(target)
            .map(|program| LinkerReport::System {
                program,
                why: Some(why.clone()),
            })
            .map_err(|e| format!("{e}\n(the bundled linker cannot be used: {why})")),
    }
}

/// What identifies the linker [`link`] would use for `target`, for build stamps: a link with
/// another linker (or another kit) must not be skipped as up to date.
pub fn linker_identity(target: &str) -> String {
    let request = bundled::Request::from_env();
    let choice = bundled::choose(request.clone(), || bundled::find(target));
    match choice {
        Ok(bundled::Choice::Bundled(b)) => bundled::identity(&b),
        _ => format!("{request:?}"),
    }
}

/// The linker program [`link`] would run for `target` (the bundled lld, or the system linker;
/// [`linker_report`] says which and why), or why none is usable. `$VELT_LINKER` is reported
/// as-is; the default unix `cc` must answer `--version`.
pub fn find_linker(target: &str) -> Result<PathBuf, String> {
    if wasm::WasmFlavor::from_triple(target).is_some() {
        return wasm::find_wasm_ld().map(|(cmd, _)| PathBuf::from(cmd.get_program()));
    }
    match linker_report(target)? {
        LinkerReport::Bundled { lld, .. } => Ok(lld),
        LinkerReport::System { program, .. } => Ok(program),
    }
}

/// `$VELT_LINKER` when it names a linker program (not the `bundled` / `system` keywords).
pub(crate) fn linker_override() -> Option<Command> {
    match bundled::Request::from_env() {
        bundled::Request::Program(p) => Some(Command::new(p)),
        _ => None,
    }
}

pub(crate) fn run_linker(mut cmd: Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .map_err(|e| format!("{LINKER_NOT_RUN} `{program}`: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let code = out
        .status
        .code()
        .map_or_else(|| "signal".to_string(), |c| c.to_string());
    let mut msg = format!("linker `{program}` failed (exit code {code})");
    for stream in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(stream);
        let text = text.trim();
        if !text.is_empty() {
            msg.push('\n');
            msg.push_str(text);
        }
    }
    Err(msg)
}

/// CONTRACT: locate the runtime library. Order: `$VELT_RT_LIB`, then next to the current
/// executable (cargo puts `velt_rt.lib`/`libvelt_rt.a` in `target/<profile>/`), then the
/// installed layout (`<prefix>/bin/velt` + `<prefix>/lib/<runtime lib>`).
pub fn find_runtime_lib(target: &str) -> Result<PathBuf, String> {
    let env = std::env::var_os("VELT_RT_LIB").filter(|s| !s.is_empty());
    let exe = std::env::current_exe().ok();
    find_runtime_lib_in(target, env.map(PathBuf::from), exe.as_deref())
}

/// Testable core of [`find_runtime_lib`]: `env` is `$VELT_RT_LIB`, `exe` the current executable.
/// Searches the exe's directory, then its parent (cargo test binaries live in `target/<profile>/deps/`),
/// then `<exe dir>/../lib` (installed toolchain).
pub fn find_runtime_lib_in(
    target: &str,
    env: Option<PathBuf>,
    exe: Option<&Path>,
) -> Result<PathBuf, String> {
    find_runtime_lib_for(target, &host_triple(), env, exe)
}

/// [`find_runtime_lib_in`] on a host `host`: the toolchain's own runtime (`lib/`, a checkout's
/// `target/<profile>/`) serves only the host target; another target's is in
/// `lib/targets/<triple>/` (its target pack).
fn find_runtime_lib_for(
    target: &str,
    host: &str,
    env: Option<PathBuf>,
    exe: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = env {
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "$VELT_RT_LIB points to `{}`, which does not exist",
                p.display()
            ))
        };
    }
    let name = runtime_lib_name(target);
    let mut searched = vec![];
    let flavor = wasm::WasmFlavor::from_triple(target);
    if let (Some(dir), Some(flavor)) = (exe.and_then(Path::parent), flavor) {
        for cand in wasm::runtime_lib_candidates(dir, flavor) {
            if cand.is_file() {
                return Ok(cand);
            }
            searched.push(cand);
        }
    } else {
        let is_host = same_target(target, host);
        for cand in native_runtime_candidates(native_search_dirs(exe), target, name, is_host) {
            if cand.is_file() {
                return Ok(cand);
            }
            searched.push(cand);
        }
    }
    let list: Vec<String> = searched
        .iter()
        .map(|p| format!("  {}", p.display()))
        .collect();
    if flavor.is_none() && !same_target(target, host) {
        // A non-host target's runtime comes with its target pack (`lib/targets/<triple>/`).
        return Err(format!(
            "this toolchain has no runtime for `{target}`; searched:\n{}\ninstall the target with \
             `velt target add {target}` (from a source checkout: `cargo build -p velt_rt --target \
             {target}`, then `velt-kit build --target {target} --runtime <it> --out \
             <prefix>/lib/targets/{target}`)",
            list.join("\n")
        ));
    }
    let build = match flavor {
        Some(f) => format!("cargo build -p velt_rt_wasm --target {}", f.rust_triple()),
        None => "cargo build -p velt_rt".into(),
    };
    Err(format!(
        "could not find the Velt runtime library `{name}`; searched:\n{}\nbuild it with `{build}` or set $VELT_RT_LIB",
        list.join("\n")
    ))
}

/// Runtime library candidates for a native target in the search directories `dirs`: first the
/// target's own directory (`<dir>/targets/<triple>/`, where a toolchain keeps the runtime of a
/// target other than the host beside its link kit), then, for the host target only, the
/// directories themselves (the host's runtime: another target's has the same file name).
fn native_runtime_candidates(
    dirs: Vec<PathBuf>,
    target: &str,
    name: &str,
    is_host: bool,
) -> Vec<PathBuf> {
    let own = kit::kit_dirs(&dirs, target)
        .into_iter()
        .map(|d| d.join(name));
    if !is_host {
        return own.collect();
    }
    own.chain(dirs.iter().map(|d| d.join(name))).collect()
}

/// Where the native runtime libraries are looked for, given the current executable: its
/// directory, then its parent (cargo test binaries live in `target/<profile>/deps/`), then
/// `<exe dir>/../lib` (installed toolchain).
pub(crate) fn native_search_dirs(exe: Option<&Path>) -> Vec<PathBuf> {
    let Some(dir) = exe.and_then(Path::parent) else {
        return vec![];
    };
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(parent) = dir.parent() {
        dirs.push(parent.to_path_buf());
        dirs.push(parent.join("lib"));
    }
    dirs
}

/// The shared runtime library for `target` (what debug builds link, see [`shared`]), found where
/// [`find_runtime_lib`] looks for the static one; `None` when it is not there, for WebAssembly,
/// when `$VELT_RT_LIB` names a specific (static) runtime, or when `$VELT_RT_LINK=static`.
pub fn find_shared_runtime_lib(target: &str) -> Option<PathBuf> {
    let forced_static = std::env::var_os("VELT_RT_LINK").is_some_and(|v| v == "static");
    let explicit = std::env::var_os("VELT_RT_LIB").is_some_and(|v| !v.is_empty());
    if forced_static || explicit {
        return None;
    }
    let exe = std::env::current_exe().ok();
    find_shared_runtime_lib_in(target, exe.as_deref())
}

/// Whether `runtime_lib` is the shared runtime for `target` (linked dynamically by [`link`]).
pub fn is_shared_runtime_lib(runtime_lib: &Path, target: &str) -> bool {
    TargetOs::from_triple(target).is_some_and(|os| shared::is_shared(runtime_lib, os))
}

/// Testable core of [`find_shared_runtime_lib`]: `exe` is the current executable.
pub fn find_shared_runtime_lib_in(target: &str, exe: Option<&Path>) -> Option<PathBuf> {
    find_shared_runtime_lib_for(target, &host_triple(), exe)
}

/// [`find_shared_runtime_lib_in`] on a host `host`: only the host target links the shared
/// runtime (a build for another target links its static runtime; musl has no shared one).
fn find_shared_runtime_lib_for(target: &str, host: &str, exe: Option<&Path>) -> Option<PathBuf> {
    if wasm::WasmFlavor::from_triple(target).is_some()
        || target.contains("musl")
        || !same_target(target, host)
    {
        return None;
    }
    let name = shared_runtime_lib_name(TargetOs::from_triple(target)?);
    native_search_dirs(exe)
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_triples() {
        assert_eq!(
            TargetOs::from_triple("x86_64-pc-windows-msvc"),
            Some(TargetOs::Windows)
        );
        assert_eq!(
            TargetOs::from_triple("aarch64-apple-darwin"),
            Some(TargetOs::MacOs)
        );
        assert_eq!(
            TargetOs::from_triple("x86_64-unknown-linux-gnu"),
            Some(TargetOs::Linux)
        );
        assert_eq!(TargetOs::from_triple("wasm32-unknown-unknown"), None);
        assert_eq!(runtime_lib_name("x86_64-pc-windows-msvc"), "velt_rt.lib");
        assert_eq!(
            runtime_lib_name("aarch64-unknown-linux-gnu"),
            "libvelt_rt.a"
        );
        assert!(find_linker("riscv64-unknown-none")
            .unwrap_err()
            .contains("unsupported target"));
        assert_eq!(runtime_lib_name("wasm32-wasip1"), "libvelt_rt_wasm.a");
    }

    #[test]
    fn rejects_cross_linking() {
        let other = match TargetOs::host() {
            TargetOs::Windows => "x86_64-unknown-linux-gnu",
            _ => "x86_64-pc-windows-msvc",
        };
        let err = link(&LinkRequest {
            target: other,
            objects: &[],
            runtime_lib: Path::new("x"),
            output: Path::new("y"),
            release: false,
            native: &[],
        })
        .unwrap_err();
        assert!(err.contains("cross-OS linking"), "{err}");
    }

    #[test]
    fn runtime_lookup_order() {
        let tmp = std::env::temp_dir().join(format!("velt_link_lookup_{}", std::process::id()));
        let deps = tmp.join("debug").join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let exe = deps.join("test-bin");

        // Not found anywhere → error lists both candidates.
        let err = find_runtime_lib_for(target, target, None, Some(&exe)).unwrap_err();
        assert!(
            err.contains("libvelt_rt.a") && err.contains("deps"),
            "{err}"
        );

        // Found in the parent of the exe dir (cargo test layout).
        let in_parent = tmp.join("debug").join("libvelt_rt.a");
        std::fs::write(&in_parent, b"").unwrap();
        assert_eq!(
            find_runtime_lib_for(target, target, None, Some(&exe)).unwrap(),
            in_parent
        );

        // Installed layout: <prefix>/bin/velt + <prefix>/lib/<runtime lib>.
        let prefix = tmp.join("prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("lib")).unwrap();
        let installed = prefix.join("lib").join("libvelt_rt.a");
        std::fs::write(&installed, b"").unwrap();
        let installed_exe = prefix.join("bin").join("velt");
        assert_eq!(
            find_runtime_lib_for(target, target, None, Some(&installed_exe)).unwrap(),
            installed
        );

        // Next to the exe wins over the parent.
        let beside = deps.join("libvelt_rt.a");
        std::fs::write(&beside, b"").unwrap();
        assert_eq!(
            find_runtime_lib_for(target, target, None, Some(&exe)).unwrap(),
            beside
        );

        // $VELT_RT_LIB wins over everything; must exist.
        let custom = tmp.join("custom.a");
        std::fs::write(&custom, b"").unwrap();
        assert_eq!(
            find_runtime_lib_for(target, target, Some(custom.clone()), Some(&exe)).unwrap(),
            custom
        );
        assert!(
            find_runtime_lib_for(target, target, Some(tmp.join("missing.a")), Some(&exe)).is_err()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_missing_system_linker_says_why_the_bundled_one_was_not_used() {
        let why = "no link kit for `x86_64-pc-windows-msvc` (looked in C:\\velt\\lib\\targets\\x86_64-pc-windows-msvc)";
        let msvc = "could not find the MSVC linker (link.exe). Install Visual Studio …".to_string();
        assert_eq!(
            with_bundled_reason(msvc.clone(), Some(why)),
            format!("{msvc}\n(the bundled linker cannot be used: {why})")
        );
        let cc = "failed to run linker `cc`: No such file or directory (os error 2)".to_string();
        assert!(with_bundled_reason(cc, Some(why)).ends_with(&format!("cannot be used: {why})")));
        // A link that ran and failed is the linker's own error; with no fallback, nothing to add.
        let undefined = "linker `cc` failed (exit code 1)\nundefined symbol: foo".to_string();
        assert_eq!(with_bundled_reason(undefined.clone(), Some(why)), undefined);
        assert_eq!(with_bundled_reason(msvc.clone(), None), msvc);
    }

    #[test]
    fn other_targets_use_their_target_packs_only() {
        let tmp = std::env::temp_dir().join(format!("velt_link_packs_{}", std::process::id()));
        let prefix = tmp.join("prefix");
        let host = "x86_64-unknown-linux-gnu";
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        // The host's runtimes; every Linux target's has the same file names.
        std::fs::create_dir_all(prefix.join("lib")).unwrap();
        std::fs::write(prefix.join("lib/libvelt_rt.a"), b"").unwrap();
        std::fs::write(prefix.join("lib/libvelt_rt_shared.so"), b"").unwrap();
        let exe = prefix.join("bin/velt");
        assert_eq!(
            find_runtime_lib_for(host, host, None, Some(&exe)).unwrap(),
            prefix.join("lib/libvelt_rt.a")
        );
        assert!(find_shared_runtime_lib_for(host, host, Some(&exe)).is_some());
        for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-gnu"] {
            let err = find_runtime_lib_for(target, host, None, Some(&exe)).unwrap_err();
            assert!(err.contains(&format!("velt target add {target}")), "{err}");
            assert_eq!(find_shared_runtime_lib_for(target, host, Some(&exe)), None);
            let pack = prefix.join("lib/targets").join(target);
            std::fs::create_dir_all(&pack).unwrap();
            std::fs::write(pack.join("libvelt_rt.a"), b"").unwrap();
            assert_eq!(
                find_runtime_lib_for(target, host, None, Some(&exe)).unwrap(),
                pack.join("libvelt_rt.a")
            );
        }
        // A Windows target's runtime is `velt_rt.lib`, in its pack too.
        let windows = "x86_64-pc-windows-msvc";
        assert!(find_runtime_lib_for(windows, host, None, Some(&exe)).is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn shared_runtime_lookup() {
        let tmp = std::env::temp_dir().join(format!("velt_link_shared_{}", std::process::id()));
        let deps = tmp.join("debug").join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let exe = deps.join("test-bin");
        assert_eq!(
            find_shared_runtime_lib_for(target, target, Some(&exe)),
            None
        );
        let lib = tmp.join("debug").join("libvelt_rt_shared.so");
        std::fs::write(&lib, b"").unwrap();
        assert_eq!(
            find_shared_runtime_lib_for(target, target, Some(&exe)),
            Some(lib)
        );
        assert_eq!(
            find_shared_runtime_lib_for("wasm32-wasip1", "wasm32-wasip1", Some(&exe)),
            None
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
