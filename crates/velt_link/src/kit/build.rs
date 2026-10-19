//! Building a link kit (`velt-kit build`, run when a toolchain is packaged, and by tests): turns
//! the checked-in sources under `crates/velt_link/kit/` into the files of [`super::Kit`]. Needs
//! an `lld` (to write import libraries and stub shared libraries) and, for Windows and glibc
//! kits, `rustc` with the target's standard library (to compile the startup objects); a musl
//! kit copies the Rust toolchain's `self-contained` musl files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use object::write::{Object, StandardSection, Symbol, SymbolSection};
use object::{Architecture, BinaryFormat, Endianness, SymbolFlags, SymbolKind, SymbolScope};

use super::{
    linux_gnu_libs, stamp_text, Arch, KitKind, MACOS_LIBS, MUSL_FILES, STAMP, WINDOWS_LIBS,
};

const CRT_WINDOWS: &str = include_str!("../../kit/crt/windows_x86_64.rs");
const CRT_LINUX_GNU: &str = include_str!("../../kit/crt/linux_gnu.rs");
const LINUX_X86_64: &str = include_str!("../../kit/linux/x86_64.txt");
const LINUX_AARCH64: &str = include_str!("../../kit/linux/aarch64.txt");
const MACOS: &str = include_str!("../../kit/macos.txt");

/// The `.def` file of each [`WINDOWS_LIBS`] entry.
fn windows_def(name: &str) -> Option<&'static str> {
    Some(match name {
        "kernel32" => include_str!("../../kit/windows/kernel32.def"),
        "ntdll" => include_str!("../../kit/windows/ntdll.def"),
        "advapi32" => include_str!("../../kit/windows/advapi32.def"),
        "ws2_32" => include_str!("../../kit/windows/ws2_32.def"),
        "bcrypt" => include_str!("../../kit/windows/bcrypt.def"),
        "userenv" => include_str!("../../kit/windows/userenv.def"),
        "dbghelp" => include_str!("../../kit/windows/dbghelp.def"),
        "secur32" => include_str!("../../kit/windows/secur32.def"),
        "psapi" => include_str!("../../kit/windows/psapi.def"),
        "shell32" => include_str!("../../kit/windows/shell32.def"),
        "user32" => include_str!("../../kit/windows/user32.def"),
        "synchronization" => include_str!("../../kit/windows/synchronization.def"),
        "ucrt" => include_str!("../../kit/windows/ucrt.def"),
        "ole32" => include_str!("../../kit/windows/ole32.def"),
        "oleaut32" => include_str!("../../kit/windows/oleaut32.def"),
        "crypt32" => include_str!("../../kit/windows/crypt32.def"),
        "ncrypt" => include_str!("../../kit/windows/ncrypt.def"),
        "iphlpapi" => include_str!("../../kit/windows/iphlpapi.def"),
        _ => return None,
    })
}

/// What to build.
pub struct Options<'a> {
    /// Target triple of the kit.
    pub target: &'a str,
    /// The kit directory to write (created; files already there are replaced).
    pub out: &'a Path,
    /// The lld to use (any flavor name; it is run with `-flavor`).
    pub lld: &'a Path,
    /// musl kits: the runtime built for the target (`libvelt_rt.a`), copied into the kit, where
    /// `velt` looks for the runtime of a non-host target.
    pub runtime: Option<&'a Path>,
}

/// Build the kit described by `opts`.
pub fn build(opts: &Options) -> Result<(), String> {
    let kind = KitKind::for_target(opts.target)
        .ok_or_else(|| format!("no link kit exists for target `{}`", opts.target))?;
    let arch = Arch::from_triple(opts.target).expect("kit kinds have an arch");
    std::fs::create_dir_all(opts.out)
        .map_err(|e| format!("cannot create {}: {e}", opts.out.display()))?;
    // An interrupted build must not leave a kit that looks complete.
    let _ = std::fs::remove_file(opts.out.join(STAMP));
    match kind {
        KitKind::WindowsMsvc => windows(opts)?,
        KitKind::LinuxGnu => linux_gnu(opts, arch)?,
        KitKind::LinuxMusl => linux_musl(opts, arch)?,
        KitKind::MacOs => macos(opts)?,
    }
    write(&opts.out.join(STAMP), stamp_text(opts.target).as_bytes())
}

fn windows(opts: &Options) -> Result<(), String> {
    let work = opts.out.join("def");
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    for (name, _) in WINDOWS_LIBS {
        let def = work.join(format!("{name}.def"));
        write(
            &def,
            windows_def(name)
                .expect("every kit library has a .def")
                .as_bytes(),
        )?;
        let mut cmd = lld(opts.lld, "link");
        cmd.arg("/lib").arg("/nologo").arg("/machine:x64");
        cmd.arg(arg("/def:", &def));
        cmd.arg(arg("/out:", &opts.out.join(format!("{name}.lib"))));
        run(cmd)?;
    }
    let _ = std::fs::remove_dir_all(&work);
    let triple = "x86_64-pc-windows-msvc";
    compile_crt(CRT_WINDOWS, triple, &[], &opts.out.join("velt_crt.obj"))?;
    compile_crt(
        CRT_WINDOWS,
        triple,
        &["--cfg", "velt_crt_dll"],
        &opts.out.join("velt_crt_dll.obj"),
    )
}

fn linux_gnu(opts: &Options, arch: Arch) -> Result<(), String> {
    let list = match arch {
        Arch::X86_64 => LINUX_X86_64,
        Arch::Aarch64 => LINUX_AARCH64,
    };
    let libs = parse_linux_list(list)?;
    let work = opts.out.join("stub-src");
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    for soname in linux_gnu_libs(arch) {
        let symbols = libs
            .get(soname)
            .ok_or_else(|| format!("kit/linux/{}.txt has no symbols of {soname}", arch.name()))?;
        let obj = work.join(format!("{soname}.o"));
        write(&obj, &elf_stub_object(arch, symbols)?)?;
        let script = work.join(format!("{soname}.map"));
        write(&script, version_script(symbols).as_bytes())?;
        let mut cmd = lld(opts.lld, "gnu");
        cmd.args(["-shared", "--hash-style=both", "-soname", soname]);
        cmd.arg(arg("--version-script=", &script));
        cmd.arg("-o").arg(opts.out.join(soname)).arg(&obj);
        run(cmd)?;
    }
    let _ = std::fs::remove_dir_all(&work);
    compile_crt(
        CRT_LINUX_GNU,
        &format!("{}-unknown-linux-gnu", arch.name()),
        &["-C", "relocation-model=pic"],
        &opts.out.join("crt1.o"),
    )
}

fn linux_musl(opts: &Options, arch: Arch) -> Result<(), String> {
    let triple = format!("{}-unknown-linux-musl", arch.name());
    let sysroot = crate::wasm::rust_sysroot().ok_or("cannot run `rustc --print sysroot`")?;
    let dir = sysroot.join(format!("lib/rustlib/{triple}/lib/self-contained"));
    if !dir.is_dir() {
        return Err(format!(
            "{} not found: run `rustup target add {triple}`",
            dir.display()
        ));
    }
    for name in MUSL_FILES {
        copy(&dir.join(name), &opts.out.join(name))?;
    }
    if let Some(runtime) = opts.runtime {
        copy(runtime, &opts.out.join(crate::runtime_lib_name(&triple)))?;
    }
    Ok(())
}

fn macos(opts: &Options) -> Result<(), String> {
    let mut symbols: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (n, line) in MACOS.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lib, sym) = line
            .split_once(' ')
            .ok_or_else(|| format!("kit/macos.txt:{}: expected `<library> <symbol>`", n + 1))?;
        symbols.entry(lib).or_default().push(sym.trim());
    }
    for (lib, path, install_name) in MACOS_LIBS {
        let syms = symbols.get(lib).map(Vec::as_slice).unwrap_or_default();
        let file = opts.out.join(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        write(&file, tbd(install_name, syms).as_bytes())?;
    }
    Ok(())
}

/// A TAPI text stub (`.tbd`, version 4) exporting `symbols` from `install_name`.
pub(crate) fn tbd(install_name: &str, symbols: &[&str]) -> String {
    let quoted: Vec<String> = symbols.iter().map(|s| format!("'{s}'")).collect();
    format!(
        "--- !tapi-tbd\n\
         tbd-version: 4\n\
         targets: [ x86_64-macos, arm64-macos ]\n\
         install-name: '{install_name}'\n\
         current-version: 1\n\
         exports:\n  - targets: [ x86_64-macos, arm64-macos ]\n    symbols: [ {} ]\n...\n",
        quoted.join(", ")
    )
}

/// One symbol of a glibc stub library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StubSymbol {
    pub name: String,
    /// Symbol version (`GLIBC_2.2.5`), `None` for an unversioned symbol.
    pub version: Option<String>,
    /// Data (an object with a size) rather than a function.
    pub data: bool,
    pub size: u64,
}

/// `kit/linux/<arch>.txt`: `<soname> <version|-> <F|O> <size> <name>` per line.
pub(crate) fn parse_linux_list(text: &str) -> Result<BTreeMap<String, Vec<StubSymbol>>, String> {
    let mut libs: BTreeMap<String, Vec<StubSymbol>> = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || format!("kit/linux list line {}: `{line}`", n + 1);
        let f: Vec<&str> = line.split_whitespace().collect();
        let [soname, version, kind, size, name] = f[..] else {
            return Err(bad());
        };
        libs.entry(soname.to_string())
            .or_default()
            .push(StubSymbol {
                name: name.to_string(),
                version: (version != "-").then(|| version.to_string()),
                data: match kind {
                    "F" => false,
                    "O" => true,
                    _ => return Err(bad()),
                },
                size: size.parse().map_err(|_| bad())?,
            });
    }
    Ok(libs)
}

/// A relocatable ELF object defining `symbols` (functions at one `ret`, data objects as zeroes
/// of their size), linked into a stub shared library.
pub(crate) fn elf_stub_object(arch: Arch, symbols: &[StubSymbol]) -> Result<Vec<u8>, String> {
    let (architecture, ret): (_, &[u8]) = match arch {
        Arch::X86_64 => (Architecture::X86_64, &[0xc3]),
        Arch::Aarch64 => (Architecture::Aarch64, &[0xc0, 0x03, 0x5f, 0xd6]),
    };
    let mut obj = Object::new(BinaryFormat::Elf, architecture, Endianness::Little);
    let text = obj.section_id(StandardSection::Text);
    let code = obj.append_section_data(text, ret, 16);
    let data = obj.section_id(StandardSection::Data);
    for s in symbols {
        let (section, value, kind) = if s.data {
            let size = usize::try_from(s.size.max(1)).map_err(|_| "symbol too large")?;
            let offset = obj.append_section_data(data, &vec![0; size], 8);
            (data, offset, SymbolKind::Data)
        } else {
            (text, code, SymbolKind::Text)
        };
        obj.add_symbol(Symbol {
            name: s.name.as_bytes().to_vec(),
            value,
            size: s.size,
            kind,
            scope: SymbolScope::Dynamic,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
    }
    obj.write()
        .map_err(|e| format!("cannot write a stub object: {e}"))
}

/// An lld version script giving each symbol its version.
pub(crate) fn version_script(symbols: &[StubSymbol]) -> String {
    let mut by_version: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in symbols {
        if let Some(v) = &s.version {
            by_version.entry(v).or_default().push(&s.name);
        }
    }
    let mut out = String::new();
    for (version, names) in by_version {
        out.push_str(version);
        out.push_str(" {\n  global:\n");
        for n in names {
            out.push_str(&format!("    {n};\n"));
        }
        out.push_str("};\n");
    }
    out
}

/// `rustc --crate-type lib --emit obj` of a `no_std` startup source for `triple`.
fn compile_crt(source: &str, triple: &str, extra: &[&str], out: &Path) -> Result<(), String> {
    let dir = out.parent().unwrap_or(Path::new("."));
    let src = dir.join(format!(
        "{}.rs",
        out.file_stem().unwrap_or_default().to_string_lossy()
    ));
    write(&src, source.as_bytes())?;
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut cmd = Command::new(rustc);
    cmd.args([
        "--edition",
        "2021",
        "--crate-type",
        "lib",
        "--crate-name",
        "velt_crt",
    ]);
    cmd.arg(arg("--emit=obj=", out));
    cmd.args(["--target", triple]);
    cmd.args([
        "-C",
        "panic=abort",
        "-C",
        "opt-level=2",
        "-C",
        "overflow-checks=off",
        "-C",
        "debug-assertions=off",
    ]);
    cmd.args(extra).arg(&src);
    let result = run(cmd).map_err(|e| {
        format!("{e}\n(the startup object needs the standard library for {triple}: `rustup target add {triple}`)")
    });
    let _ = std::fs::remove_file(&src);
    result
}

/// `lld -flavor <flavor>`.
fn lld(lld: &Path, flavor: &str) -> Command {
    let mut cmd = Command::new(lld);
    cmd.args(["-flavor", flavor]);
    cmd
}

fn arg(prefix: &str, path: &Path) -> std::ffi::OsString {
    let mut a = std::ffi::OsString::from(prefix);
    a.push(path);
    a
}

fn run(cmd: Command) -> Result<(), String> {
    crate::run_linker(cmd)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// The lld a kit is built with when none is given: the bundled one next to this executable,
/// else the Rust toolchain's `rust-lld`.
pub fn default_lld() -> Option<PathBuf> {
    crate::bundled::find_lld()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_lists_parse() {
        let libs = parse_linux_list(
            "# comment\nlibc.so.6 GLIBC_2.2.5 F 0 malloc\nlibc.so.6 GLIBC_2.2.5 O 8 stdout\nlibgcc_s.so.1 - F 0 x\n",
        )
        .unwrap();
        let libc = &libs["libc.so.6"];
        assert_eq!(libc.len(), 2);
        assert!(libc[1].data && libc[1].size == 8);
        assert_eq!(libs["libgcc_s.so.1"][0].version, None);
        assert!(parse_linux_list("libc.so.6 X Q 0 y").is_err());
        assert!(parse_linux_list("too few").is_err());
    }

    #[test]
    fn version_scripts_group_by_version() {
        let s = |name: &str, v: Option<&str>| StubSymbol {
            name: name.into(),
            version: v.map(Into::into),
            data: false,
            size: 0,
        };
        let script = version_script(&[
            s("b", Some("GLIBC_2.3")),
            s("a", Some("GLIBC_2.2.5")),
            s("c", Some("GLIBC_2.2.5")),
            s("u", None),
        ]);
        assert_eq!(
            script,
            "GLIBC_2.2.5 {\n  global:\n    a;\n    c;\n};\nGLIBC_2.3 {\n  global:\n    b;\n};\n"
        );
    }

    #[test]
    fn stub_objects_define_their_symbols() {
        use object::{Object as _, ObjectSymbol};
        let syms = [
            StubSymbol {
                name: "f".into(),
                version: None,
                data: false,
                size: 0,
            },
            StubSymbol {
                name: "d".into(),
                version: None,
                data: true,
                size: 24,
            },
        ];
        let bytes = elf_stub_object(Arch::Aarch64, &syms).unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let found: Vec<(String, u64)> = file
            .symbols()
            .filter(|s| s.is_global() && !s.is_undefined())
            .map(|s| (s.name().unwrap().to_string(), s.size()))
            .collect();
        assert_eq!(found, vec![("f".into(), 0), ("d".into(), 24)]);
    }

    #[test]
    fn tbd_lists_symbols() {
        let text = tbd("/usr/lib/libSystem.B.dylib", &["_malloc", "_free"]);
        assert!(text.contains("install-name: '/usr/lib/libSystem.B.dylib'"));
        assert!(text.contains("symbols: [ '_malloc', '_free' ]"));
    }
}
