//! Link kits: what the bundled `lld` needs besides the program, the runtime and the system's own
//! shared libraries to link an executable for one target, so `velt build` needs no Visual Studio
//! Build Tools, Xcode or `cc` (docs/internals/linking.md).
//!
//! A kit is a directory `<prefix>/lib/targets/<triple>/`:
//!
//! - **Windows (MSVC):** import libraries for the system DLLs the runtime imports and for the
//!   Universal CRT (`ucrt.lib` → `ucrtbase.dll`), generated from `kit/windows/*.def`, plus
//!   `velt_crt.obj` / `velt_crt_dll.obj`: the startup code Visual Studio's CRT objects provide
//!   (`kit/crt/windows_x86_64.rs`).
//! - **Linux (glibc):** stub shared libraries (`libc.so.6`, …) that define the symbols and
//!   symbol versions of glibc 2.31 (`kit/linux/<arch>.txt`), and `crt1.o`
//!   (`kit/crt/linux_gnu.rs`). Executables run on any glibc from 2.31 on.
//! - **Linux (musl):** musl's crt objects and `libc.a` plus `libunwind.a` (the `self-contained`
//!   copies the Rust toolchain ships), and the runtime built for musl: fully static executables.
//! - **macOS:** `.tbd` text stubs for `libSystem` and the two frameworks the runtime uses
//!   (`kit/macos.txt`), laid out as an SDK (`-syslibroot`).
//!
//! The kit's files are written by [`build`] (`velt-kit build`, run when a toolchain is
//! packaged); `kit.stamp`, written last, marks a complete kit.

use std::path::{Path, PathBuf};

pub mod build;

/// Version of the kit layout; a kit with another `kit.stamp` is not used.
pub const FORMAT: u32 = 2;

/// The velt a kit belongs to: its version, and for a build from a git checkout its commit
/// (`0.1.0+1a2b3c4d`). A kit or target pack holds a runtime built by that velt; another velt's
/// runtime may not match what its compiler emits, so [`Kit::open`] refuses it.
pub fn toolchain_id() -> &'static str {
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const HASH: &str = env!("VELT_GIT_HASH");
    if HASH.is_empty() {
        VERSION
    } else {
        concat!(env!("CARGO_PKG_VERSION"), "+", env!("VELT_GIT_HASH"))
    }
}

/// The file that marks a complete kit.
pub const STAMP: &str = "kit.stamp";

/// The kinds of kits, one per C library a target links against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KitKind {
    /// `x86_64-pc-windows-msvc`.
    WindowsMsvc,
    /// `{x86_64,aarch64}-unknown-linux-gnu`.
    LinuxGnu,
    /// `{x86_64,aarch64}-unknown-linux-musl`.
    LinuxMusl,
    /// `{x86_64,aarch64}-apple-darwin`.
    MacOs,
}

/// CPU architectures with kits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    pub fn from_triple(target: &str) -> Option<Arch> {
        match target.split('-').next()? {
            "x86_64" => Some(Arch::X86_64),
            "aarch64" | "arm64" => Some(Arch::Aarch64),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }
}

impl KitKind {
    /// The kit kind for `target`, `None` for targets without kits (they link with the system
    /// linker only).
    pub fn for_target(target: &str) -> Option<KitKind> {
        let arch = Arch::from_triple(target)?;
        if target.contains("windows") {
            (arch == Arch::X86_64 && target.ends_with("msvc")).then_some(KitKind::WindowsMsvc)
        } else if target.contains("apple") || target.contains("darwin") {
            Some(KitKind::MacOs)
        } else if target.contains("linux") {
            Some(if target.contains("musl") {
                KitKind::LinuxMusl
            } else {
                KitKind::LinuxGnu
            })
        } else {
            None
        }
    }
}

/// The Windows import libraries in a kit, by the DLL they import from. The runtime's needs
/// (`crates/velt_rt/NATIVE_LIBS.md`) first, then a few more native packages commonly use.
pub const WINDOWS_LIBS: &[(&str, &str)] = &[
    ("kernel32", "kernel32.dll"),
    ("ntdll", "ntdll.dll"),
    ("advapi32", "advapi32.dll"),
    ("ws2_32", "ws2_32.dll"),
    ("bcrypt", "bcrypt.dll"),
    ("userenv", "userenv.dll"),
    ("dbghelp", "dbghelp.dll"),
    ("secur32", "secur32.dll"),
    ("psapi", "psapi.dll"),
    ("shell32", "shell32.dll"),
    ("user32", "user32.dll"),
    ("synchronization", "api-ms-win-core-synch-l1-2-0.dll"),
    ("ucrt", "ucrtbase.dll"),
    ("ole32", "ole32.dll"),
    ("oleaut32", "oleaut32.dll"),
    ("crypt32", "crypt32.dll"),
    ("ncrypt", "ncrypt.dll"),
    ("iphlpapi", "iphlpapi.dll"),
];

/// The import libraries an executable links (all of them: lld only reads the ones it needs).
pub(crate) fn windows_link_libs() -> impl Iterator<Item = String> {
    WINDOWS_LIBS.iter().map(|(name, _)| format!("{name}.lib"))
}

/// The glibc stub libraries of a Linux kit, in link order (that of `cc` with the runtime's
/// `-lgcc_s -lutil -lrt -lpthread -lm -ldl -lc`), by soname.
pub fn linux_gnu_libs(arch: Arch) -> [&'static str; 8] {
    [
        "libgcc_s.so.1",
        "libutil.so.1",
        "librt.so.1",
        "libpthread.so.0",
        "libm.so.6",
        "libdl.so.2",
        "libc.so.6",
        linux_dynamic_loader(arch).1,
    ]
}

/// The dynamic loader of glibc executables: (path recorded in the executable, soname).
pub fn linux_dynamic_loader(arch: Arch) -> (&'static str, &'static str) {
    match arch {
        Arch::X86_64 => ("/lib64/ld-linux-x86-64.so.2", "ld-linux-x86-64.so.2"),
        Arch::Aarch64 => ("/lib/ld-linux-aarch64.so.1", "ld-linux-aarch64.so.1"),
    }
}

/// musl's startup objects and libraries in a kit (copied from the Rust toolchain).
pub const MUSL_FILES: &[&str] = &[
    "crt1.o",
    "crti.o",
    "crtbegin.o",
    "crtend.o",
    "crtn.o",
    "libc.a",
    "libunwind.a",
];

/// The macOS libraries of a kit: (name in `kit/macos.txt`, path in the kit, install name).
pub const MACOS_LIBS: &[(&str, &str, &str)] = &[
    (
        "libSystem",
        "usr/lib/libSystem.tbd",
        "/usr/lib/libSystem.B.dylib",
    ),
    (
        "CoreFoundation",
        "System/Library/Frameworks/CoreFoundation.framework/CoreFoundation.tbd",
        "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    ),
    (
        "SystemConfiguration",
        "System/Library/Frameworks/SystemConfiguration.framework/SystemConfiguration.tbd",
        "/System/Library/Frameworks/SystemConfiguration.framework/Versions/A/SystemConfiguration",
    ),
];

/// A complete kit for one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kit {
    pub dir: PathBuf,
    pub kind: KitKind,
    pub arch: Arch,
}

impl Kit {
    /// The kit in `dir` for `target`, if it is complete (`kit.stamp` of this [`FORMAT`] and
    /// [`toolchain_id`], and every file the link uses); otherwise what is wrong with it.
    pub fn open(dir: &Path, target: &str) -> Result<Kit, String> {
        Kit::open_for(dir, target, toolchain_id())
    }

    /// [`Kit::open`] for the velt `toolchain` (tests).
    pub(crate) fn open_for(dir: &Path, target: &str, toolchain: &str) -> Result<Kit, String> {
        let kind = KitKind::for_target(target)
            .ok_or_else(|| format!("no link kit exists for target `{target}`"))?;
        let arch = Arch::from_triple(target).expect("kit kinds have an arch");
        let stamp = dir.join(STAMP);
        let text = std::fs::read_to_string(&stamp)
            .map_err(|_| format!("no link kit in {}", dir.display()))?;
        let fields: Vec<&str> = text.split_whitespace().collect();
        let fix = format!("run `velt target add {target}` again, or reinstall the toolchain");
        if fields.get(1) != Some(&FORMAT.to_string().as_str()) {
            return Err(format!(
                "the link kit in {} has another format than this velt; {fix}",
                dir.display()
            ));
        }
        match fields.get(3) {
            Some(made_by) if *made_by == toolchain => {}
            made_by => {
                return Err(format!(
                    "the link kit in {} belongs to velt {}, not to this velt ({toolchain}); {fix}",
                    dir.display(),
                    made_by.unwrap_or(&"(unknown)")
                ))
            }
        }
        let kit = Kit {
            dir: dir.to_path_buf(),
            kind,
            arch,
        };
        if let Some(missing) = kit.required_files().into_iter().find(|f| !f.is_file()) {
            return Err(format!(
                "the link kit in {} is incomplete: {} is missing",
                dir.display(),
                missing.display()
            ));
        }
        Ok(kit)
    }

    /// Every file a link with this kit reads.
    pub fn required_files(&self) -> Vec<PathBuf> {
        let names: Vec<String> = match self.kind {
            KitKind::WindowsMsvc => ["velt_crt.obj", "velt_crt_dll.obj"]
                .into_iter()
                .map(String::from)
                .chain(windows_link_libs())
                .collect(),
            KitKind::LinuxGnu => std::iter::once("crt1.o")
                .chain(linux_gnu_libs(self.arch))
                .map(String::from)
                .collect(),
            KitKind::LinuxMusl => MUSL_FILES.iter().map(|f| f.to_string()).collect(),
            KitKind::MacOs => MACOS_LIBS.iter().map(|(_, p, _)| p.to_string()).collect(),
        };
        names.iter().map(|n| self.dir.join(n)).collect()
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// The contents of `kit.stamp` for `target`.
pub fn stamp_text(target: &str) -> String {
    format!("velt-kit {FORMAT} {target} {}\n", toolchain_id())
}

/// The targets releases publish a target pack for (`velt target add`): the toolchains' hosts,
/// and static musl Linux.
pub const RELEASE_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

/// Where this toolchain installs target packs: `<prefix>/lib/targets` beside
/// `<prefix>/bin/velt` (`target/lib/targets` for a checkout's `target/<profile>/velt`), which
/// [`kit_dirs`] searches.
pub fn targets_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join("lib").join("targets"))
}

/// Where kits are looked for, given the directories the runtime is searched in
/// (`<prefix>/lib` and, in a checkout, `target/` and `target/lib`): `<dir>/targets/<triple>`.
pub(crate) fn kit_dirs(search_dirs: &[PathBuf], target: &str) -> Vec<PathBuf> {
    search_dirs
        .iter()
        .map(|d| d.join("targets").join(target))
        .collect()
}

/// The first complete kit for `target` in `dirs`, or why there is none (the reason for the
/// first directory that has a kit at all, else "not found" with the places searched).
pub(crate) fn find_in(dirs: &[PathBuf], target: &str) -> Result<Kit, String> {
    let mut first_error = None;
    for d in dirs {
        if !d.is_dir() {
            continue;
        }
        match Kit::open(d, target) {
            Ok(kit) => return Ok(kit),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| {
        let list: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        format!("no link kit for `{target}` (looked in {})", list.join(", "))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_target() {
        use KitKind::*;
        let cases = [
            ("x86_64-pc-windows-msvc", Some(WindowsMsvc)),
            ("aarch64-pc-windows-msvc", None),
            ("x86_64-pc-windows-gnu", None),
            ("x86_64-unknown-linux-gnu", Some(LinuxGnu)),
            ("aarch64-unknown-linux-gnu", Some(LinuxGnu)),
            ("x86_64-unknown-linux-musl", Some(LinuxMusl)),
            ("aarch64-apple-darwin", Some(MacOs)),
            ("x86_64-apple-darwin", Some(MacOs)),
            ("riscv64gc-unknown-linux-gnu", None),
            ("wasm32-wasip1", None),
        ];
        for (target, kind) in cases {
            assert_eq!(KitKind::for_target(target), kind, "{target}");
        }
    }

    #[test]
    fn open_checks_stamp_and_files() {
        let dir = std::env::temp_dir().join(format!("velt-kit-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("usr/lib")).unwrap();
        let target = "aarch64-apple-darwin";
        assert!(Kit::open(&dir, target).unwrap_err().contains("no link kit"));
        std::fs::write(dir.join(STAMP), "velt-kit 0 aarch64-apple-darwin\n").unwrap();
        assert!(Kit::open(&dir, target)
            .unwrap_err()
            .contains("another format"));
        std::fs::write(dir.join(STAMP), stamp_text(target)).unwrap();
        let err = Kit::open(&dir, target).unwrap_err();
        assert!(
            err.contains("incomplete") && err.contains("libSystem.tbd"),
            "{err}"
        );
        for (_, path, _) in MACOS_LIBS {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let kit = Kit::open(&dir, target).unwrap();
        assert_eq!((kit.kind, kit.arch), (KitKind::MacOs, Arch::Aarch64));
        let found = find_in(&[dir.join("nope"), dir.clone()], target).unwrap();
        assert_eq!(found.dir, dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn find_reports_searched_dirs() {
        let err = find_in(
            &[PathBuf::from("/nonexistent/a")],
            "x86_64-unknown-linux-gnu",
        )
        .unwrap_err();
        assert!(err.contains("/nonexistent/a"), "{err}");
    }
}
