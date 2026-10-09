//! Arguments for the bundled lld, per object format: `lld-link` (COFF), `ld.lld` (ELF, glibc or
//! musl) and `ld64.lld` (Mach-O). They link what `link.exe` / `cc` would, with the system
//! libraries and startup objects taken from the kit ([`crate::kit`]) instead of the SDKs.

use std::ffi::OsString;
use std::path::Path;

use crate::kit::{self, Kit, KitKind};
use crate::{native, shared, LinkRequest, TargetOs};

/// The `-flavor` for a kit.
pub(crate) fn flavor(kind: KitKind) -> &'static str {
    match kind {
        KitKind::WindowsMsvc => "link",
        KitKind::LinuxGnu | KitKind::LinuxMusl => "gnu",
        KitKind::MacOs => "darwin",
    }
}

/// The arguments after `-flavor` for linking `req` with `kit`.
pub(crate) fn args(req: &LinkRequest, kit: &Kit) -> Result<Vec<OsString>, String> {
    match kit.kind {
        KitKind::WindowsMsvc => coff_args(req, kit),
        KitKind::LinuxGnu => Ok(elf_gnu_args(req, kit)),
        KitKind::LinuxMusl => elf_musl_args(req, kit),
        KitKind::MacOs => Ok(macho_args(req, kit)),
    }
}

fn path_arg(prefix: &str, path: &Path) -> OsString {
    let mut a = OsString::from(prefix);
    a.push(path);
    a
}

/// `lld-link`: like `link.exe` with the MSVC libraries (`msvc_args`), but without default
/// libraries: the kit's startup object and import libraries stand in for them.
fn coff_args(req: &LinkRequest, kit: &Kit) -> Result<Vec<OsString>, String> {
    let mut args: Vec<OsString> = [
        "/NOLOGO",
        "/NODEFAULTLIB",
        "/SUBSYSTEM:CONSOLE",
        "/INCREMENTAL:NO",
    ]
    .map(OsString::from)
    .into();
    if req.release {
        args.extend(["/OPT:REF", "/OPT:ICF"].map(OsString::from));
    } else {
        args.push("/DEBUG".into());
    }
    args.push(path_arg("/OUT:", req.output));
    args.push(path_arg("/LIBPATH:", &kit.dir));
    args.push(kit.file("velt_crt.obj").into());
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::msvc_args(req.native)?);
    args.push(req.runtime_lib.into());
    args.extend(kit::windows_link_libs().map(OsString::from));
    Ok(args)
}

/// `ld.lld` for glibc: a position-independent executable against the kit's stub libraries,
/// started by the kit's `crt1.o`.
fn elf_gnu_args(req: &LinkRequest, kit: &Kit) -> Vec<OsString> {
    let (loader, _) = kit::linux_dynamic_loader(kit.arch);
    let mut args = elf_common_args(req);
    args.extend(["-pie", "--dynamic-linker", loader].map(OsString::from));
    // The runtime's shared library refers to glibc symbols the stubs may not list for the
    // version it was built against; the dynamic loader resolves them at run time.
    args.push("--allow-shlib-undefined".into());
    args.extend(["-o".into(), req.output.into()]);
    args.push(kit.file("crt1.o").into());
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::static_objects(req.native));
    if shared::is_shared(req.runtime_lib, TargetOs::Linux) {
        args.extend(shared::lld_args(req.runtime_lib));
    } else {
        args.push(req.runtime_lib.into());
    }
    args.extend(native::lld_shared_args(req.native));
    args.push("--as-needed".into());
    args.extend(kit::linux_gnu_libs(kit.arch).map(|lib| kit.file(lib).into()));
    args.push("--no-as-needed".into());
    args
}

/// `ld.lld` for musl: a static executable (musl's crt objects, the runtime built for musl,
/// libunwind and libc from the kit). Programs cannot load shared libraries.
fn elf_musl_args(req: &LinkRequest, kit: &Kit) -> Result<Vec<OsString>, String> {
    if req.native.iter().any(|n| n.static_obj.is_none()) {
        return Err(
            "musl executables are static: native libraries of packages need a prelinked object \
             (release builds), not a shared library"
                .into(),
        );
    }
    let mut args = elf_common_args(req);
    args.extend(["-static", "-no-pie"].map(OsString::from));
    args.extend(["-o".into(), req.output.into()]);
    for f in ["crt1.o", "crti.o", "crtbegin.o"] {
        args.push(kit.file(f).into());
    }
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::static_objects(req.native));
    args.push("--start-group".into());
    args.push(req.runtime_lib.into());
    args.push(kit.file("libunwind.a").into());
    args.push(kit.file("libc.a").into());
    args.push("--end-group".into());
    for f in ["crtend.o", "crtn.o"] {
        args.push(kit.file(f).into());
    }
    Ok(args)
}

/// What `cc` passes `ld` on every ELF link, plus the release settings.
fn elf_common_args(req: &LinkRequest) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--eh-frame-hdr",
        "--hash-style=gnu",
        "--build-id",
        "-z",
        "relro",
        "-z",
        "noexecstack",
    ]
    .map(OsString::from)
    .into();
    if req.release {
        args.extend(["--gc-sections", "-s"].map(OsString::from));
    }
    args
}

/// `ld64.lld` with the kit as the SDK (`-syslibroot`): `-lSystem` and the frameworks resolve
/// to its `.tbd` stubs. lld signs arm64 executables (ad hoc) itself.
fn macho_args(req: &LinkRequest, kit: &Kit) -> Vec<OsString> {
    let requested = std::env::var("MACOSX_DEPLOYMENT_TARGET").ok();
    let version = crate::macos_min_version(req.target, requested.as_deref());
    let mut args: Vec<OsString> = vec![
        "-arch".into(),
        crate::macos_arch(req.target).into(),
        "-platform_version".into(),
        "macos".into(),
        version.clone().into(),
        // The SDK version: the stubs stand for no particular one; claim the deployment target.
        version.into(),
        "-syslibroot".into(),
        kit.dir.clone().into(),
    ];
    if req.release {
        args.push("-dead_strip".into());
    }
    args.extend(["-o".into(), req.output.into()]);
    args.extend(req.objects.iter().map(OsString::from));
    args.extend(native::static_objects(req.native));
    if shared::is_shared(req.runtime_lib, TargetOs::MacOs) {
        args.extend(shared::lld_args(req.runtime_lib));
    } else {
        args.push(req.runtime_lib.into());
    }
    args.extend(native::lld_shared_args(req.native));
    args.extend(
        [
            "-lSystem",
            "-framework",
            "CoreFoundation",
            "-framework",
            "SystemConfiguration",
        ]
        .map(OsString::from),
    );
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::Arch;
    use std::path::PathBuf;

    fn kit(kind: KitKind, arch: Arch) -> Kit {
        Kit {
            dir: PathBuf::from("K"),
            kind,
            arch,
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn req<'a>(
        target: &'a str,
        objects: &'a [PathBuf],
        runtime: &'a Path,
        release: bool,
    ) -> LinkRequest<'a> {
        LinkRequest {
            target,
            objects,
            runtime_lib: runtime,
            output: Path::new("out"),
            release,
            native: &[],
        }
    }

    fn pos(args: &[String], item: &str) -> usize {
        args.iter()
            .position(|a| a == item || a.ends_with(item))
            .unwrap_or_else(|| panic!("{item} not in {args:?}"))
    }

    #[test]
    fn coff() {
        let objs = [PathBuf::from("main.obj")];
        let k = kit(KitKind::WindowsMsvc, Arch::X86_64);
        let a = strings(
            &args(
                &req(
                    "x86_64-pc-windows-msvc",
                    &objs,
                    Path::new("velt_rt.lib"),
                    false,
                ),
                &k,
            )
            .unwrap(),
        );
        for item in [
            "/NODEFAULTLIB",
            "/DEBUG",
            "/OUT:out",
            "ucrt.lib",
            "kernel32.lib",
        ] {
            pos(&a, item);
        }
        assert!(a.contains(&"/LIBPATH:K".to_string()), "{a:?}");
        // startup object, program, runtime, then the import libraries
        assert!(pos(&a, "velt_crt.obj") < pos(&a, "main.obj"));
        assert!(pos(&a, "main.obj") < pos(&a, "velt_rt.lib"));
        assert!(pos(&a, "velt_rt.lib") < pos(&a, "kernel32.lib"));
        let r = strings(
            &args(
                &req(
                    "x86_64-pc-windows-msvc",
                    &objs,
                    Path::new("velt_rt.lib"),
                    true,
                ),
                &k,
            )
            .unwrap(),
        );
        assert!(r.contains(&"/OPT:REF".to_string()) && !r.contains(&"/DEBUG".to_string()));
    }

    #[test]
    fn elf_gnu() {
        let objs = [PathBuf::from("main.o")];
        let k = kit(KitKind::LinuxGnu, Arch::X86_64);
        let a = strings(
            &args(
                &req(
                    "x86_64-unknown-linux-gnu",
                    &objs,
                    Path::new("libvelt_rt.a"),
                    true,
                ),
                &k,
            )
            .unwrap(),
        );
        assert_eq!(
            a[pos(&a, "--dynamic-linker") + 1],
            "/lib64/ld-linux-x86-64.so.2"
        );
        for item in ["-pie", "--eh-frame-hdr", "--gc-sections", "-s"] {
            pos(&a, item);
        }
        assert!(pos(&a, "crt1.o") < pos(&a, "main.o"));
        assert!(pos(&a, "main.o") < pos(&a, "libvelt_rt.a"));
        assert!(pos(&a, "libvelt_rt.a") < pos(&a, "--as-needed"));
        assert!(pos(&a, "libgcc_s.so.1") < pos(&a, "libc.so.6"));
        let k = kit(KitKind::LinuxGnu, Arch::Aarch64);
        let a = strings(
            &args(
                &req(
                    "aarch64-unknown-linux-gnu",
                    &objs,
                    Path::new("libvelt_rt.a"),
                    false,
                ),
                &k,
            )
            .unwrap(),
        );
        assert_eq!(
            a[pos(&a, "--dynamic-linker") + 1],
            "/lib/ld-linux-aarch64.so.1"
        );
        assert!(!a.contains(&"-s".to_string()));
    }

    #[test]
    fn elf_shared_runtime_uses_rpath() {
        let objs = [PathBuf::from("main.o")];
        let k = kit(KitKind::LinuxGnu, Arch::X86_64);
        let a = strings(
            &args(
                &req(
                    "x86_64-unknown-linux-gnu",
                    &objs,
                    Path::new("/rt/libvelt_rt_shared.so"),
                    false,
                ),
                &k,
            )
            .unwrap(),
        );
        assert!(a.contains(&"-lvelt_rt_shared".to_string()), "{a:?}");
        assert!(a.contains(&"-rpath".to_string()), "{a:?}");
    }

    #[test]
    fn elf_musl() {
        let objs = [PathBuf::from("main.o")];
        let k = kit(KitKind::LinuxMusl, Arch::X86_64);
        let a = strings(
            &args(
                &req(
                    "x86_64-unknown-linux-musl",
                    &objs,
                    Path::new("K/libvelt_rt.a"),
                    false,
                ),
                &k,
            )
            .unwrap(),
        );
        for item in ["-static", "-no-pie", "--start-group", "--end-group"] {
            pos(&a, item);
        }
        assert!(pos(&a, "crt1.o") < pos(&a, "main.o"));
        assert!(pos(&a, "libunwind.a") < pos(&a, "libc.a"));
        assert!(pos(&a, "libc.a") < pos(&a, "crtn.o"));
        assert!(!a.iter().any(|x| x.contains("dynamic-linker")));
    }

    #[test]
    fn macho() {
        let objs = [PathBuf::from("main.o")];
        let k = kit(KitKind::MacOs, Arch::Aarch64);
        let a = strings(
            &args(
                &req(
                    "aarch64-apple-darwin",
                    &objs,
                    Path::new("libvelt_rt.a"),
                    true,
                ),
                &k,
            )
            .unwrap(),
        );
        assert_eq!(a[pos(&a, "-arch") + 1], "arm64");
        assert_eq!(a[pos(&a, "-syslibroot") + 1], "K");
        let pv = pos(&a, "-platform_version");
        assert_eq!(a[pv + 1], "macos");
        pos(&a, "-dead_strip");
        assert!(pos(&a, "libvelt_rt.a") < pos(&a, "-lSystem"));
        let k = kit(KitKind::MacOs, Arch::X86_64);
        let a = strings(
            &args(
                &req(
                    "x86_64-apple-darwin",
                    &objs,
                    Path::new("libvelt_rt.a"),
                    false,
                ),
                &k,
            )
            .unwrap(),
        );
        assert_eq!(a[pos(&a, "-arch") + 1], "x86_64");
    }
}
