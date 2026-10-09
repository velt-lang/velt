//! The system linker: MSVC's `link.exe` (found with `cc::windows_registry::find_tool`, which reads
//! the registry / vswhere and returns the `LIB`/`PATH` environment for the MSVC + Windows SDK
//! libraries, so no "Developer Command Prompt" is needed) or the C compiler driver `cc`, with each
//! platform's native libraries. [`crate::link`] uses it when the bundled linker is not there or
//! `$VELT_LINKER` asks for it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::{linker_override, native, shared, LinkRequest, TargetOs};

/// How the error starts when there is no MSVC linker (see [`crate::with_bundled_reason`]).
pub(crate) const NO_MSVC_LINKER: &str = "could not find the MSVC linker";

/// Per-platform link settings. Native libraries are what the Rust `std` (and later tokio) inside the
/// velt_rt staticlib needs — cross-checked with
/// `cargo rustc -p velt_rt --crate-type staticlib -- --print native-static-libs`.
pub(crate) struct Platform {
    pub(crate) runtime_lib_name: &'static str,
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

pub(crate) fn platform(os: TargetOs) -> &'static Platform {
    match os {
        TargetOs::Windows => &WINDOWS,
        TargetOs::Linux => &LINUX,
        TargetOs::MacOs => &MACOS,
    }
}

/// The system linker program for `target` (`$VELT_LINKER` as-is, `link.exe` or `cc`).
pub(crate) fn find_system_linker(target: &str) -> Result<PathBuf, String> {
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
pub(crate) fn macos_arch(target: &str) -> &'static str {
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
pub(crate) fn macos_min_version(target: &str, requested: Option<&str>) -> String {
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

pub(crate) fn unix_linker() -> Command {
    linker_override().unwrap_or_else(|| Command::new("cc"))
}

#[cfg(windows)]
pub(crate) fn msvc_linker(target: &str) -> Result<Command, String> {
    if let Some(cmd) = linker_override() {
        return Ok(cmd);
    }
    // `find_tool` wants an MSVC target triple; normalize e.g. `x86_64-windows` spellings.
    let arch = target.split('-').next().unwrap_or("x86_64");
    let triple = format!("{arch}-pc-windows-msvc");
    let tool = cc::windows_registry::find_tool(&triple, "link.exe").ok_or_else(|| {
        format!(
            "{NO_MSVC_LINKER} (link.exe). Install Visual Studio or the \"Build Tools for Visual \
             Studio\" with the \"Desktop development with C++\" workload (MSVC + Windows SDK), or \
             set $VELT_LINKER to link.exe"
        )
    })?;
    // `to_command` applies the LIB / PATH / INCLUDE environment that find_tool discovered.
    Ok(tool.to_command())
}

#[cfg(not(windows))]
pub(crate) fn msvc_linker(_target: &str) -> Result<Command, String> {
    Err("linking for Windows requires a Windows host".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
