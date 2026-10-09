//! `velt-kit`: builds link kits and refreshes the lists they are built from
//! (docs/internals/linking.md).
//!
//! ```text
//! velt-kit build --target <triple> --out <dir> [--lld <path>] [--runtime <libvelt_rt.a>]
//! velt-kit lists linux   [--out <file>] [--root <dir>] [--arch <arch>]
//!                                                from glibc 2.31 (Debian 11): this system's,
//!                                                or the libraries of an image unpacked in <dir>
//! velt-kit lists windows [--out <dir>]           on Windows: System32 DLL exports
//! velt-kit lists macos --runtime <libvelt_rt.a> [--out <file>]
//!                                                on macOS with Xcode: merges this arch's imports
//! ```
//!
//! Run with `cargo run -p velt_link --bin velt-kit -- …` (the packaging scripts do).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use velt_link::kit::{self, Arch};

const USAGE: &str = "usage:
  velt-kit build --target <triple> --out <dir> [--lld <path>] [--runtime <file>]
  velt-kit lists linux [--out <file>] [--root <dir>] [--arch x86_64|aarch64]
  velt-kit lists windows [--out <dir>]
  velt-kit lists macos --runtime <libvelt_rt.a> [--out <file>]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("velt-kit: {e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Flags {
    target: Option<String>,
    out: Option<PathBuf>,
    lld: Option<PathBuf>,
    runtime: Option<PathBuf>,
    root: Option<PathBuf>,
    arch: Option<String>,
}

fn flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--target" => f.target = Some(value()?),
            "--out" => f.out = Some(value()?.into()),
            "--lld" => f.lld = Some(value()?.into()),
            "--runtime" => f.runtime = Some(value()?.into()),
            "--root" => f.root = Some(value()?.into()),
            "--arch" => f.arch = Some(value()?),
            other => return Err(format!("unknown option `{other}`\n{USAGE}")),
        }
    }
    Ok(f)
}

fn run(args: Vec<String>) -> Result<(), String> {
    let repo_kit = Path::new(env!("CARGO_MANIFEST_DIR")).join("kit");
    match args.first().map(String::as_str) {
        Some("build") => {
            let f = flags(&args[1..])?;
            let target = f.target.ok_or("--target is required")?;
            let out = f.out.ok_or("--out is required")?;
            let lld = match f.lld {
                Some(p) => p,
                None => kit::build::default_lld()
                    .ok_or("no lld found: pass --lld, or install Rust (its rust-lld)")?,
            };
            kit::build::build(&kit::build::Options {
                target: &target,
                out: &out,
                lld: &lld,
                runtime: f.runtime.as_deref(),
            })?;
            println!("kit for {target} in {}", out.display());
            Ok(())
        }
        Some("lists") => {
            let f = flags(&args[2..])?;
            match args.get(1).map(String::as_str) {
                Some("linux") => {
                    let arch = Arch::from_triple(f.arch.as_deref().unwrap_or(std::env::consts::ARCH))
                        .ok_or("unsupported architecture")?;
                    let out = f
                        .out
                        .unwrap_or_else(|| repo_kit.join(format!("linux/{}.txt", arch.name())));
                    let root = f.root.unwrap_or_else(|| PathBuf::from("/"));
                    write(&out, &linux_list(arch, &root)?)
                }
                Some("windows") => {
                    windows_lists(&f.out.unwrap_or_else(|| repo_kit.join("windows")))
                }
                Some("macos") => {
                    let runtime = f.runtime.ok_or("--runtime is required")?;
                    let out = f.out.unwrap_or_else(|| repo_kit.join("macos.txt"));
                    let merged = macos_list(&runtime, std::fs::read_to_string(&out).ok())?;
                    write(&out, &merged)
                }
                _ => Err(USAGE.into()),
            }
        }
        _ => Err(USAGE.into()),
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---- Linux ----------------------------------------------------------------------------------

/// Every exported symbol of the kit's glibc libraries on this system, at its default version
/// (`readelf --dyn-syms`): the stubs then accept whatever a program or a native package's object
/// calls, not only what the runtime does.
fn linux_list(arch: Arch, root: &Path) -> Result<String, String> {
    let multiarch = format!("{}-linux-gnu", arch.name());
    let dirs = [
        root.join(format!("lib/{multiarch}")),
        root.join(format!("usr/lib/{multiarch}")),
        root.join("lib64"),
        root.join("lib"),
    ];
    // libc.so.6 names its version in a string ("GNU C Library (Debian GLIBC 2.31-13...)").
    let libc = dirs
        .iter()
        .map(|d| d.join("libc.so.6"))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("libc.so.6 not found under {}", root.display()))?;
    let bytes = std::fs::read(&libc).map_err(|e| format!("{}: {e}", libc.display()))?;
    let marker = b"GNU C Library";
    let glibc = bytes
        .windows(marker.len())
        .position(|w| w == marker)
        .map(|at| {
            let rest = &bytes[at..];
            let end = rest.iter().position(|&b| b == b'\n' || b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(&rest[..end]).trim_end_matches('.').to_string()
        })
        .unwrap_or_else(|| "glibc (version unknown)".into());
    let mut out = format!(
        "# Exported symbols of the glibc libraries the bundled linker's stubs stand for:\n\
         # <soname> <version|-> <F|O> <size> <name>. Generated by `velt-kit lists linux` from\n\
         # {glibc} ({}).\n",
        arch.name()
    );
    for soname in kit::linux_gnu_libs(arch) {
        let path = dirs
            .iter()
            .map(|d| Path::new(d).join(soname))
            .find(|p| p.is_file())
            .ok_or_else(|| format!("{soname} not found under {}", root.display()))?;
        let text = output(
            Command::new("readelf")
                .args(["--dyn-syms", "-W"])
                .arg(&path),
        )?;
        let mut seen = BTreeSet::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            // Num: Value Size Type Bind Vis Ndx Name
            if f.len() < 8 || !f[0].ends_with(':') {
                continue;
            }
            let (size, kind, bind, ndx, name) = (f[2], f[3], f[4], f[6], f[7]);
            if ndx == "UND" || ndx == "ABS" || !matches!(bind, "GLOBAL" | "WEAK" | "UNIQUE") {
                continue;
            }
            let data = match kind {
                "FUNC" | "IFUNC" => false,
                "OBJECT" => true,
                _ => continue,
            };
            let (name, version) = match name.split_once("@@") {
                Some((n, v)) => (n, v),
                // A non-default (compatibility) version: new links cannot use it.
                None if name.contains('@') => continue,
                None => (name, "-"),
            };
            if version == "GLIBC_PRIVATE" || !seen.insert(name.to_string()) {
                continue;
            }
            let size: u64 = if let Some(hex) = size.strip_prefix("0x") {
                u64::from_str_radix(hex, 16).unwrap_or(0)
            } else {
                size.parse().unwrap_or(0)
            };
            let kind = if data { "O" } else { "F" };
            out.push_str(&format!(
                "{soname} {version} {kind} {} {name}\n",
                if data { size } else { 0 }
            ));
        }
    }
    Ok(out)
}

// ---- Windows --------------------------------------------------------------------------------

/// `.def` files with every export of the kit's DLLs on this system; exports outside executable
/// sections are marked `DATA` (imported through `__imp_` only).
fn windows_lists(out: &Path) -> Result<(), String> {
    use object::read::pe::PeFile64;
    use object::LittleEndian as LE;
    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let system32 =
        PathBuf::from(std::env::var_os("WINDIR").ok_or("$WINDIR is not set")?).join("System32");
    for (name, dll) in kit::WINDOWS_LIBS {
        // API sets (`api-ms-win-*`) are not files: list their functions by hand.
        if dll.starts_with("api-ms-win-core-synch") {
            write(
                &out.join(format!("{name}.def")),
                &format!(
                    "LIBRARY {dll}\nEXPORTS\n  WaitOnAddress\n  WakeByAddressAll\n  WakeByAddressSingle\n"
                ),
            )?;
            continue;
        }
        let path = system32.join(dll);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let pe = PeFile64::parse(&*bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let exports = pe
            .export_table()
            .map_err(|e| format!("{}: {e}", path.display()))?
            .ok_or_else(|| format!("{} exports nothing", path.display()))?;
        let sections = pe.section_table();
        let mut lines = BTreeMap::new();
        for (name, ordinal) in exports.name_iter() {
            let Ok(name) = std::str::from_utf8(exports.name_from_pointer(name).unwrap_or(&[]))
            else {
                continue;
            };
            let data = match exports.target_by_index(ordinal.into()) {
                Ok(object::read::pe::ExportTarget::Address(rva)) => {
                    sections.section_containing(rva).is_some_and(|s| {
                        s.characteristics.get(LE) & object::pe::IMAGE_SCN_MEM_EXECUTE == 0
                    })
                }
                _ => false,
            };
            lines.insert(name.to_string(), data);
        }
        let mut text = format!("LIBRARY {dll}\nEXPORTS\n");
        for (n, data) in lines {
            text.push_str(&format!("  {n}{}\n", if data { " DATA" } else { "" }));
        }
        write(&out.join(format!("{name}.def")), &text)?;
    }
    Ok(())
}

// ---- macOS ----------------------------------------------------------------------------------

/// The symbols the whole runtime imports from the kit's libraries (link it with `-all_load`
/// against the SDK and read the binds), merged with `existing` (the other architecture's).
fn macos_list(runtime: &Path, existing: Option<String>) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("velt-kit-macos-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let exe = dir.join("all");
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "arm64"
    };
    let mut link = Command::new("cc");
    link.args(["-arch", arch, "-o"]).arg(&exe);
    // The program's own symbols (`velt_main`, …) stay undefined; the rest binds to the
    // libraries `nm -m` then names.
    link.arg("-Wl,-all_load").arg(runtime).arg("-Wl,-undefined,dynamic_lookup");
    link.args([
        "-framework",
        "SystemConfiguration",
        "-framework",
        "CoreFoundation",
        "-liconv",
        "-lm",
    ]);
    output(&mut link)?;
    let text = output(Command::new("nm").arg("-m").arg(&exe))?;
    let _ = std::fs::remove_dir_all(&dir);
    let mut set: BTreeSet<(String, String)> = BTreeSet::new();
    for line in existing.iter().flat_map(|t| t.lines()) {
        if let Some((lib, sym)) = line.split_once(' ').filter(|_| !line.starts_with('#')) {
            set.insert((lib.to_string(), sym.trim().to_string()));
        }
    }
    for line in text.lines().filter(|l| l.contains("(undefined)")) {
        // (undefined) external _foo (from libSystem)
        let Some(sym) = line.split_whitespace().nth(2) else {
            continue;
        };
        let Some(from) = line
            .rsplit_once("(from ")
            .map(|(_, f)| f.trim_end_matches(')'))
        else {
            continue;
        };
        let lib = kit::MACOS_LIBS
            .iter()
            .find(|(name, _, _)| *name == from)
            .map(|(name, _, _)| *name)
            .ok_or_else(|| format!("the runtime imports {sym} from {from}, which has no stub"))?;
        set.insert((lib.to_string(), sym.to_string()));
    }
    let mut out = String::from(
        "# Symbols the runtime imports from macOS system libraries (`<library> <symbol>`), from\n\
         # which the bundled linker's .tbd stubs are written. Generated by `velt-kit lists macos`\n\
         # on arm64 and x86_64 (each run merges its architecture's imports).\n",
    );
    for (lib, sym) in set {
        out.push_str(&format!("{lib} {sym}\n"));
    }
    Ok(out)
}
