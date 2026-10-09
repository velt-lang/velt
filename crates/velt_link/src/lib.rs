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

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod bundled;
mod fast_ld;
pub mod kit;
mod lld;
mod native;
mod runtime_profile;
mod shared;
pub mod wasm;

pub use native::NativeLink;
pub use runtime_profile::runtime_lib_is_debug;
pub use shared::shared_runtime_lib_name;

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

/// Per-platform link settings. Native libraries are what the Rust `std` (and later tokio) inside the
/// velt_rt staticlib needs — cross-checked with
/// `cargo rustc -p velt_rt --crate-type staticlib -- --print native-static-libs`.
struct Platform {
    runtime_lib_name: &'static str,
    native_libs: &'static [&'static str],
    /// Always-on linker arguments.
    base_args: &'static [&'static str],
    debug_args: &'static [&'static str],
    release_args: &'static [&'static str],
}

const WINDOWS: Platform = Platform {
    runtime_lib_name: "velt_rt.lib",
    // Rust links the dynamic CRT (/MD) by default on msvc → msvcrt.lib (which pulls in
    // vcruntime.lib + ucrt.lib via /DEFAULTLIB directives).
    native_libs: &[
        "kernel32.lib",
        "advapi32.lib",
        "ntdll.lib",
        "userenv.lib",
        "ws2_32.lib",
        "bcrypt.lib",
        "dbghelp.lib",
        "synchronization.lib",
        // `whoami` (tokio-postgres' default user name).
        "secur32.lib",
        "msvcrt.lib",
    ],
    base_args: &["/NOLOGO", "/SUBSYSTEM:CONSOLE", "/INCREMENTAL:NO"],
    debug_args: &["/DEBUG"],
    release_args: &["/OPT:REF", "/OPT:ICF"],
};

const LINUX: Platform = Platform {
    runtime_lib_name: "libvelt_rt.a",
    native_libs: &[
        "-lgcc_s",
        "-lutil",
        "-lrt",
        "-lpthread",
        "-lm",
        "-ldl",
        "-lc",
    ],
    base_args: &["-pie"],
    debug_args: &[],
    release_args: &["-Wl,--gc-sections", "-s"],
};

const MACOS: Platform = Platform {
    runtime_lib_name: "libvelt_rt.a",
    // rustc's list minus `-lSystem`: `cc` always links libSystem, and passing it again makes
    // ld64 print "ignoring duplicate libraries" into every link error.
    native_libs: &[
        "-framework",
        "SystemConfiguration",
        "-framework",
        "CoreFoundation",
        "-liconv",
        "-lc",
        "-lm",
    ],
    base_args: &[],
    debug_args: &[],
    release_args: &["-Wl,-dead_strip"],
};

fn platform(os: TargetOs) -> &'static Platform {
    match os {
        TargetOs::Windows => &WINDOWS,
        TargetOs::Linux => &LINUX,
        TargetOs::MacOs => &MACOS,
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
    let shared = shared::is_shared(req.runtime_lib, os);
    match (choice, os) {
        (bundled::Choice::Bundled(b), _) => link_with_bundled(req, &b, os),
        (_, TargetOs::Windows) => {
            let mut cmd = msvc_linker(req.target)?;
            cmd.args(msvc_args(req)?);
            run_linker(cmd)?;
            if shared {
                shared::place_dll(req.runtime_lib, req.output)?;
            }
            native::place_dlls(req.native, req.output)
        }
        (_, TargetOs::Linux | TargetOs::MacOs) => {
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

/// The system linker program for `target` (`$VELT_LINKER` as-is, `link.exe` or `cc`).
fn find_system_linker(target: &str) -> Result<PathBuf, String> {
    let os =
        TargetOs::from_triple(target).ok_or_else(|| format!("unsupported target `{target}`"))?;
    let cmd = match os {
        TargetOs::Windows => msvc_linker(target)?,
        TargetOs::Linux | TargetOs::MacOs => {
            let cmd = unix_linker();
            if linker_override().is_none()
                && !Command::new(cmd.get_program())
                    .arg("--version")
                    .output()
                    .is_ok_and(|o| o.status.success())
            {
                return Err("could not run the system C compiler `cc`, which Velt uses as the linker. Install it (Debian/Ubuntu: `sudo apt install build-essential`; Fedora: `sudo dnf install gcc`; macOS: `xcode-select --install`), or set $VELT_LINKER"
                    .into());
            }
            cmd
        }
    };
    Ok(PathBuf::from(cmd.get_program()))
}

pub(crate) fn msvc_args(req: &LinkRequest) -> Result<Vec<OsString>, String> {
    let p = &WINDOWS;
    let mut args: Vec<OsString> = p.base_args.iter().map(OsString::from).collect();
    let extra = if req.release {
        p.release_args
    } else {
        p.debug_args
    };
    args.extend(extra.iter().map(OsString::from));
    let mut out = OsString::from("/OUT:");
    out.push(req.output);
    args.push(out);
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::msvc_args(req.native)?);
    args.push(req.runtime_lib.into());
    args.extend(p.native_libs.iter().map(OsString::from));
    Ok(args)
}

pub(crate) fn unix_args(req: &LinkRequest, os: TargetOs) -> Vec<OsString> {
    let p = platform(os);
    let mut args: Vec<OsString> = p.base_args.iter().map(OsString::from).collect();
    if os == TargetOs::MacOs {
        args.extend(["-arch", macos_arch(req.target)].map(OsString::from));
        let requested = std::env::var("MACOSX_DEPLOYMENT_TARGET").ok();
        let version = macos_min_version(req.target, requested.as_deref());
        args.push(format!("-mmacosx-version-min={version}").into());
    }
    let extra = if req.release {
        p.release_args
    } else {
        p.debug_args
    };
    args.extend(extra.iter().map(OsString::from));
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::static_objects(req.native));
    if shared::is_shared(req.runtime_lib, os) {
        args.extend(shared::unix_args(req.runtime_lib));
    } else {
        args.push(req.runtime_lib.into());
    }
    args.extend(native::unix_shared_args(req.native));
    args.extend(p.native_libs.iter().map(OsString::from));
    args.push("-o".into());
    args.push(req.output.into());
    args
}

/// `cc` on macOS is a universal driver that links for the architecture the *calling process*
/// runs as, so an arm64 `velt` would link x86_64 objects as arm64 (and fail) without this.
fn macos_arch(target: &str) -> &'static str {
    if target.starts_with("x86_64") {
        "x86_64"
    } else {
        "arm64"
    }
}

/// Oldest macOS the executable runs on: `$MACOSX_DEPLOYMENT_TARGET` (as for rustc and clang),
/// raised to the runtime's minimum, else that minimum. Without the flag `cc` stamps the build
/// machine's OS version into `LC_BUILD_VERSION`, and programs refuse to start on older systems.
/// The minimums are rustc's (so the runtime library's) and the Cranelift objects': 11.0 on
/// arm64, 10.12 on x86_64. An unparsable value is ignored, like a missing one.
fn macos_min_version(target: &str, requested: Option<&str>) -> String {
    let (floor, floor_text) = if target.starts_with("x86_64") {
        ((10, 12, 0), "10.12")
    } else {
        ((11, 0, 0), "11.0")
    };
    let version = requested
        .map(str::trim)
        .filter(|v| parse_macos_version(v).is_some_and(|v| v > floor))
        .unwrap_or(floor_text);
    version.to_string()
}

/// `major[.minor[.patch]]`.
fn parse_macos_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let mut next = || parts.next().map(str::parse::<u32>).transpose().ok();
    let version = (next()??, next()?.unwrap_or(0), next()?.unwrap_or(0));
    parts.next().is_none().then_some(version)
}

/// `$VELT_LINKER` when it names a linker program (not the `bundled` / `system` keywords).
pub(crate) fn linker_override() -> Option<Command> {
    match bundled::Request::from_env() {
        bundled::Request::Program(p) => Some(Command::new(p)),
        _ => None,
    }
}

fn unix_linker() -> Command {
    linker_override().unwrap_or_else(|| Command::new("cc"))
}

#[cfg(windows)]
fn msvc_linker(target: &str) -> Result<Command, String> {
    if let Some(cmd) = linker_override() {
        return Ok(cmd);
    }
    // `find_tool` wants an MSVC target triple; normalize e.g. `x86_64-windows` spellings.
    let arch = target.split('-').next().unwrap_or("x86_64");
    let triple = format!("{arch}-pc-windows-msvc");
    let tool = cc::windows_registry::find_tool(&triple, "link.exe").ok_or_else(|| {
        "could not find the MSVC linker (link.exe). Install Visual Studio or the \"Build Tools for Visual \
         Studio\" with the \"Desktop development with C++\" workload (MSVC + Windows SDK), or set \
         $VELT_LINKER to link.exe"
            .to_string()
    })?;
    // `to_command` applies the LIB / PATH / INCLUDE environment that find_tool discovered.
    Ok(tool.to_command())
}

#[cfg(not(windows))]
fn msvc_linker(_target: &str) -> Result<Command, String> {
    Err("linking for Windows requires a Windows host".into())
}

pub(crate) fn run_linker(mut cmd: Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run linker `{program}`: {e}"))?;
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
        for cand in native_runtime_candidates(native_search_dirs(exe), target, name) {
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
    let build = match flavor {
        Some(f) => format!("cargo build -p velt_rt_wasm --target {}", f.rust_triple()),
        // A non-host target's runtime lives in its kit (`lib/targets/<triple>/`).
        None if target.contains("musl") => format!(
            "cargo build -p velt_rt --target {target}` plus `velt-kit build --target {target} \
             --runtime <it> --out <prefix>/lib/targets/{target}"
        ),
        None => "cargo build -p velt_rt".into(),
    };
    Err(format!(
        "could not find the Velt runtime library `{name}`; searched:\n{}\nbuild it with `{build}` or set $VELT_RT_LIB",
        list.join("\n")
    ))
}

/// Runtime library candidates for a native target in the search directories `dirs`: first the
/// target's own directory (`<dir>/targets/<triple>/`, where a toolchain keeps the runtime of a
/// target other than the host, e.g. musl, beside its link kit), then the directories themselves
/// (the host's runtime). musl uses only the former: the host's glibc runtime has the same name.
fn native_runtime_candidates(dirs: Vec<PathBuf>, target: &str, name: &str) -> Vec<PathBuf> {
    let own = kit::kit_dirs(&dirs, target)
        .into_iter()
        .map(|d| d.join(name));
    if target.contains("musl") {
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
    if wasm::WasmFlavor::from_triple(target).is_some() || target.contains("musl") {
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
    fn macos_links_for_the_oldest_supported_version() {
        let args = |target| {
            let req = LinkRequest {
                target,
                objects: &[],
                runtime_lib: Path::new("rt"),
                output: Path::new("out"),
                release: false,
                native: &[],
            };
            let os = TargetOs::from_triple(target).unwrap();
            unix_args(&req, os)
        };
        let min = |target| {
            args(target)
                .into_iter()
                .find(|a| a.to_string_lossy().starts_with("-mmacosx-version-min="))
        };
        assert_eq!(
            min("aarch64-apple-darwin"),
            Some("-mmacosx-version-min=11.0".into())
        );
        assert_eq!(
            min("x86_64-apple-darwin"),
            Some("-mmacosx-version-min=10.12".into())
        );
        assert_eq!(min("aarch64-unknown-linux-gnu"), None);
    }

    #[test]
    fn macos_deployment_target_is_honored_above_the_minimum() {
        let min = |t, r| format!("-mmacosx-version-min={}", macos_min_version(t, r));
        assert_eq!(
            min("aarch64-apple-darwin", Some("13.4")),
            "-mmacosx-version-min=13.4"
        );
        assert_eq!(
            min("x86_64-apple-darwin", Some("10.15")),
            "-mmacosx-version-min=10.15"
        );
        // Older than the runtime supports, or not a version: the runtime's minimum.
        assert_eq!(
            min("aarch64-apple-darwin", Some("10.15")),
            "-mmacosx-version-min=11.0"
        );
        assert_eq!(
            min("x86_64-apple-darwin", Some("10.9")),
            "-mmacosx-version-min=10.12"
        );
        assert_eq!(
            min("aarch64-apple-darwin", Some("latest")),
            "-mmacosx-version-min=11.0"
        );
        assert_eq!(
            min("aarch64-apple-darwin", Some("")),
            "-mmacosx-version-min=11.0"
        );
        assert_eq!(parse_macos_version("12"), Some((12, 0, 0)));
        assert_eq!(parse_macos_version("12.3.1"), Some((12, 3, 1)));
        assert_eq!(parse_macos_version("12.3.1.4"), None);
        assert_eq!(parse_macos_version("12."), None);
    }

    #[test]
    fn macos_links_for_the_target_arch() {
        let arch = |target| {
            let req = LinkRequest {
                target,
                objects: &[],
                runtime_lib: Path::new("rt"),
                output: Path::new("out"),
                release: false,
                native: &[],
            };
            let a = unix_args(&req, TargetOs::from_triple(target).unwrap());
            let i = a.iter().position(|s| s == "-arch")?;
            Some(a[i + 1].to_string_lossy().into_owned())
        };
        assert_eq!(arch("x86_64-apple-darwin").as_deref(), Some("x86_64"));
        assert_eq!(arch("aarch64-apple-darwin").as_deref(), Some("arm64"));
        assert_eq!(arch("arm64-apple-macosx11.0").as_deref(), Some("arm64"));
        assert_eq!(arch("aarch64-unknown-linux-gnu"), None);
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
        let err = find_runtime_lib_in(target, None, Some(&exe)).unwrap_err();
        assert!(
            err.contains("libvelt_rt.a") && err.contains("deps"),
            "{err}"
        );

        // Found in the parent of the exe dir (cargo test layout).
        let in_parent = tmp.join("debug").join("libvelt_rt.a");
        std::fs::write(&in_parent, b"").unwrap();
        assert_eq!(
            find_runtime_lib_in(target, None, Some(&exe)).unwrap(),
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
            find_runtime_lib_in(target, None, Some(&installed_exe)).unwrap(),
            installed
        );

        // Next to the exe wins over the parent.
        let beside = deps.join("libvelt_rt.a");
        std::fs::write(&beside, b"").unwrap();
        assert_eq!(
            find_runtime_lib_in(target, None, Some(&exe)).unwrap(),
            beside
        );

        // $VELT_RT_LIB wins over everything; must exist.
        let custom = tmp.join("custom.a");
        std::fs::write(&custom, b"").unwrap();
        assert_eq!(
            find_runtime_lib_in(target, Some(custom.clone()), Some(&exe)).unwrap(),
            custom
        );
        assert!(find_runtime_lib_in(target, Some(tmp.join("missing.a")), Some(&exe)).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn musl_runtime_comes_from_its_target_directory() {
        let tmp = std::env::temp_dir().join(format!("velt_link_musl_{}", std::process::id()));
        let prefix = tmp.join("prefix");
        let target = "x86_64-unknown-linux-musl";
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("lib/targets").join(target)).unwrap();
        // The host's (glibc) runtime has the same name and must not be taken.
        std::fs::write(prefix.join("lib/libvelt_rt.a"), b"").unwrap();
        let exe = prefix.join("bin/velt");
        let err = find_runtime_lib_in(target, None, Some(&exe)).unwrap_err();
        assert!(err.contains("--target x86_64-unknown-linux-musl"), "{err}");
        let own = prefix.join("lib/targets").join(target).join("libvelt_rt.a");
        std::fs::write(&own, b"").unwrap();
        assert_eq!(find_runtime_lib_in(target, None, Some(&exe)).unwrap(), own);
        // Other targets prefer their own directory, then the host's.
        let gnu = "x86_64-unknown-linux-gnu";
        assert_eq!(
            find_runtime_lib_in(gnu, None, Some(&exe)).unwrap(),
            prefix.join("lib/libvelt_rt.a")
        );
        assert_eq!(find_shared_runtime_lib_in(target, Some(&exe)), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn shared_runtime_lookup() {
        let tmp = std::env::temp_dir().join(format!("velt_link_shared_{}", std::process::id()));
        let deps = tmp.join("debug").join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let target = "x86_64-unknown-linux-gnu";
        let exe = deps.join("test-bin");
        assert_eq!(find_shared_runtime_lib_in(target, Some(&exe)), None);
        let lib = tmp.join("debug").join("libvelt_rt_shared.so");
        std::fs::write(&lib, b"").unwrap();
        assert_eq!(find_shared_runtime_lib_in(target, Some(&exe)), Some(lib));
        assert_eq!(
            find_shared_runtime_lib_in("wasm32-wasip1", Some(&exe)),
            None
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arg_lists() {
        let objs = [PathBuf::from("main.obj")];
        let req = LinkRequest {
            target: "x86_64-pc-windows-msvc",
            objects: &objs,
            runtime_lib: Path::new("velt_rt.lib"),
            output: Path::new("out.exe"),
            release: false,
            native: &[],
        };
        let a: Vec<String> = msvc_args(&req)
            .unwrap()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(a.contains(&"/OUT:out.exe".to_string()));
        assert!(a.contains(&"/DEBUG".to_string()));
        assert!(a.contains(&"msvcrt.lib".to_string()));
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap();
        assert!(pos("main.obj") < pos("velt_rt.lib") && pos("velt_rt.lib") < pos("kernel32.lib"));

        let req = LinkRequest {
            release: true,
            native: &[],
            target: "x86_64-unknown-linux-gnu",
            runtime_lib: Path::new("libvelt_rt.a"),
            output: Path::new("out"),
            ..req
        };
        let a: Vec<String> = unix_args(&req, TargetOs::Linux)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&a[a.len() - 2..], &["-o".to_string(), "out".to_string()]);
        assert!(a.contains(&"-lpthread".to_string()) && a.contains(&"-s".to_string()));
    }
}
