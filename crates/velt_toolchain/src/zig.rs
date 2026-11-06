//! The pinned zig toolchain (docs/internals/design/c-ffi.md "Checking declarations against C
//! headers", "Where libraries come from"): its clang parses C headers for every target, with
//! zig's own libc headers, so a package author's build can check `declare c` functions against
//! them, and `zig cc` builds a package's C `sources` for every target from one machine. It is
//! downloaded on first use into `$VELT_HOME/cache/zig/<version>/`, verified against the checksum
//! below, and kept; `$VELT_ZIG` names a zig of the same version to use instead (offline machines,
//! CI). Users installing a published package never run it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

/// The zig release whose clang checks headers and builds C sources. A new one changes
/// [`ARCHIVES`] too.
pub const VERSION: &str = "0.17.1";

/// zig's archive per host (`<arch>-<os>`, as zig names them), and its SHA-256
/// (https://ziglang.org/download/index.json).
const ARCHIVES: &[(&str, &str)] = &[
    (
        "aarch64-macos",
        "75f1bc7a1e9852f17ec9a0c1895f767c4ec56d46bc8a834920eece1d0359dd8d",
    ),
    (
        "x86_64-macos",
        "9af07b20319fbd0c92ee46ad23e81d6a782b1aaea2cfb8dcbff35f1d2c7ddbb7",
    ),
    (
        "x86_64-linux",
        "3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a",
    ),
    (
        "aarch64-linux",
        "a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795",
    ),
    (
        "x86_64-windows",
        "b03f477a1943673e02aca0f570e8030dffd536ce1f45cbc4d3326ec31159562e",
    ),
    (
        "aarch64-windows",
        "4789895bdb1e312a2ea73b2a7c0a0cc8d2ab62ba6976e834a7c54cf443377c15",
    ),
];

/// The most a zig archive may be (they are about 100 MiB at most).
const MAX_ARCHIVE: usize = 256 << 20;

/// This machine as zig names hosts (`aarch64-macos`); `None` on one zig has no release for.
fn host() -> Option<&'static str> {
    let host = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-macos",
        ("x86_64", "macos") => "x86_64-macos",
        ("x86_64", "linux") => "x86_64-linux",
        ("aarch64", "linux") => "aarch64-linux",
        ("x86_64", "windows") => "x86_64-windows",
        ("aarch64", "windows") => "aarch64-windows",
        _ => return None,
    };
    Some(host)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "zig.exe"
    } else {
        "zig"
    }
}

/// The zig executable to run: `$VELT_ZIG`, or the pinned release in `home`'s cache, downloaded
/// and unpacked first if it isn't there. `Err` says why there is none.
pub fn zig(home: &Path) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("VELT_ZIG") {
        return check_version(PathBuf::from(path));
    }
    let Some(host) = host() else {
        return Err(format!(
            "C headers and sources need zig {VERSION}, which has no release for this machine ({}-{}): set VELT_ZIG to a zig {VERSION}",
            std::env::consts::ARCH,
            std::env::consts::OS
        ));
    };
    let dir = home.join("cache").join("zig").join(VERSION);
    let exe = dir.join(format!("zig-{host}-{VERSION}")).join(exe_name());
    if exe.is_file() {
        return Ok(exe);
    }
    let (_, sha) = ARCHIVES
        .iter()
        .find(|(h, _)| *h == host)
        .ok_or_else(|| format!("ICE: no checksum of zig {VERSION} for {host}"))?;
    let ext = if host.ends_with("windows") {
        "zip"
    } else {
        "tar.xz"
    };
    let url = format!("https://ziglang.org/download/{VERSION}/zig-{host}-{VERSION}.{ext}");
    eprintln!("Downloading zig {VERSION} for C headers and sources (once): {url}");
    let bytes = crate::install::download(&url, MAX_ARCHIVE)?
        .ok_or_else(|| format!("{url} does not exist"))?;
    verify(&bytes, sha, &url)?;
    let top = format!("zig-{host}-{VERSION}");
    install(&dir, &top, &exe, |into| match ext {
        "zip" => unzip(&bytes, into),
        _ => untar_xz(&bytes, into),
    })
    .map_err(|e| format!("{url}: {e}"))
}

/// Put the tree `top` in `dir`, as `unpack` writes it into the directory it is given, and return
/// `exe` in it. Unpacked next to its final place and renamed, so a cut-off unpack is never used.
/// Each call unpacks into a directory of its own, also beside other threads of this process: an
/// in-process compile of several programs at once installs on each of them.
fn install(
    dir: &Path,
    top: &str,
    exe: &Path,
    unpack: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let partial = dir.join(format!(".unpacking-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    if let Err(e) = unpack(&partial) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("unpacking: {e}"));
    }
    match std::fs::rename(partial.join(top), dir.join(top)) {
        Ok(()) => {}
        // Another build unpacked it meanwhile.
        Err(_) if exe.is_file() => {}
        Err(e) => return Err(format!("{}: {e}", dir.join(top).display())),
    }
    let _ = std::fs::remove_dir_all(&partial);
    if !exe.is_file() {
        return Err(format!("no {top}/{}", exe_name()));
    }
    Ok(exe.to_path_buf())
}

/// The target as zig names it, for reading its C headers; `None` for one without them
/// (WebAssembly). Windows targets read zig's MinGW headers, which have the MSVC targets' type
/// sizes.
pub fn target(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-apple-darwin" => "x86_64-macos",
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        t if t.starts_with("x86_64-pc-windows") => "x86_64-windows-gnu",
        t if t.starts_with("aarch64-pc-windows") => "aarch64-windows-gnu",
        _ => return None,
    })
}

/// The target as zig names it, for building C sources: [`target`], with the oldest system the
/// runtime supports where the name says one, so a library neither raises what a program needs nor
/// depends on the machine it was built on. `None` for one without a C library (WebAssembly).
pub fn build_target(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "aarch64-apple-darwin" => "aarch64-macos.11.0",
        "x86_64-apple-darwin" => "x86_64-macos.10.12",
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu.2.17",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu.2.17",
        _ => return target(triple),
    })
}

/// `Err` unless `bytes` hash to `sha` (hex).
fn verify(bytes: &[u8], sha: &str, what: &str) -> Result<(), String> {
    let got = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != sha {
        return Err(format!(
            "{what}: SHA-256 is {got}, not the pinned {sha}; refusing to use it"
        ));
    }
    Ok(())
}

/// `path` if it is a zig of [`VERSION`].
fn check_version(path: PathBuf) -> Result<PathBuf, String> {
    let out = std::process::Command::new(&path)
        .arg("version")
        .output()
        .map_err(|e| format!("VELT_ZIG={}: {e}", path.display()))?;
    if !out.status.success() {
        return Err(format!(
            "VELT_ZIG={}: `zig version` failed ({}): {}",
            path.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version != VERSION {
        return Err(format!(
            "VELT_ZIG={} is zig {version}; headers are checked with zig {VERSION}",
            path.display()
        ));
    }
    Ok(path)
}

fn untar_xz(bytes: &[u8], into: &Path) -> Result<(), String> {
    let mut tar = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(bytes), &mut tar)
        .map_err(|e| format!("not an xz archive: {e}"))?;
    let mut archive = tar::Archive::new(tar.as_slice());
    archive.set_preserve_permissions(true);
    archive.unpack(into).map_err(|e| e.to_string())
}

fn unzip(bytes: &[u8], into: &Path) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        // `enclosed_name` refuses absolute paths and `..`.
        let Some(rel) = file.enclosed_name() else {
            return Err(format!(
                "{}: a path outside the archive",
                String::from_utf8_lossy(file.name_raw())
            ));
        };
        let out = into.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        std::fs::write(&out, data).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_targets_name_the_oldest_system_the_runtime_supports() {
        assert_eq!(
            build_target("aarch64-apple-darwin"),
            Some("aarch64-macos.11.0")
        );
        assert_eq!(
            build_target("x86_64-apple-darwin"),
            Some("x86_64-macos.10.12")
        );
        assert_eq!(
            build_target("x86_64-unknown-linux-gnu"),
            Some("x86_64-linux-gnu.2.17")
        );
        assert_eq!(
            build_target("x86_64-unknown-linux-musl"),
            Some("x86_64-linux-musl")
        );
        assert_eq!(
            build_target("x86_64-pc-windows-msvc"),
            Some("x86_64-windows-gnu")
        );
        assert_eq!(build_target("wasm32-wasip1"), None);
    }

    #[test]
    fn every_host_has_a_checksum() {
        if let Some(host) = host() {
            assert!(ARCHIVES.iter().any(|(h, _)| *h == host), "{host}");
        }
        for (h, sha) in ARCHIVES {
            assert_eq!(sha.len(), 64, "{h}");
        }
    }

    /// A zig that can't run says why, not "is zig ``" with an empty version.
    #[cfg(unix)]
    #[test]
    fn a_zig_that_fails_to_run_says_why() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zig");
        std::fs::write(&path, "#!/bin/sh\necho no loader >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = check_version(path).unwrap_err();
        assert!(
            e.contains("`zig version` failed") && e.contains("no loader"),
            "{e}"
        );
    }

    #[test]
    fn a_wrong_checksum_is_refused() {
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify(b"hello", sha, "x").is_ok());
        let e = verify(b"hellp", sha, "zig.tar.xz").unwrap_err();
        assert!(e.contains("not the pinned"), "{e}");
    }

    /// Two installs at once in one process, as when it compiles programs on several threads
    /// (veltc's `bench_programs` test, on eight): the second one's start must not delete what the
    /// first has unpacked so far. It did, since both unpacked into one directory named by the
    /// process, and the first renamed a tree without the files deleted under it (CI's zig lost
    /// `lib/include/__stddef_size_t.h` and `__stdarg_header_macro.h` that way).
    #[test]
    fn installs_at_once_in_one_process_each_unpack_their_own_tree() {
        use std::sync::mpsc::channel;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let exe = dir.join("top/f0");
        let write = |into: &Path, files: std::ops::Range<usize>| -> Result<(), String> {
            std::fs::create_dir_all(into.join("top")).map_err(|e| e.to_string())?;
            for i in files {
                std::fs::write(into.join(format!("top/f{i}")), b"x").map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let (half, at_half) = channel();
        let (started, has_started) = channel();
        let (done, is_done) = channel();
        let (exe, write) = (&exe, &write);
        std::thread::scope(|s| {
            let first = s.spawn(move || {
                let installed = install(dir, "top", exe, |into| {
                    write(into, 0..10)?;
                    half.send(()).unwrap();
                    has_started.recv().unwrap();
                    write(into, 10..20)
                });
                done.send(()).unwrap();
                installed
            });
            let second = s.spawn(move || {
                at_half.recv().unwrap();
                install(dir, "top", exe, |into| {
                    write(into, 0..3)?;
                    started.send(()).unwrap();
                    is_done.recv().unwrap();
                    write(into, 3..20)
                })
            });
            assert_eq!(first.join().unwrap(), Ok(exe.clone()));
            assert_eq!(second.join().unwrap(), Ok(exe.clone()));
        });
        let missing: Vec<usize> = (0..20)
            .filter(|i| !dir.join(format!("top/f{i}")).is_file())
            .collect();
        assert!(missing.is_empty(), "missing from the tree: {missing:?}");
        // Neither left its unpacking directory behind.
        let left: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["top"]);
    }

    #[test]
    fn archives_unpack() {
        let dir = std::env::temp_dir().join(format!("velt-zig-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A zip holding `zig-x/zig`.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::SimpleFileOptions = Default::default();
            w.start_file("zig-x/zig", opts).unwrap();
            std::io::Write::write_all(&mut w, b"binary").unwrap();
            w.finish().unwrap();
        }
        unzip(buf.get_ref(), &dir).unwrap();
        assert_eq!(std::fs::read(dir.join("zig-x/zig")).unwrap(), b"binary");
        // A tar.xz holding `zig-y/zig`.
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "zig-y/zig", &b"exe"[..])
            .unwrap();
        let tar = tar.into_inner().unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut tar.as_slice(), &mut xz).unwrap();
        untar_xz(&xz, &dir).unwrap();
        assert_eq!(std::fs::read(dir.join("zig-y/zig")).unwrap(), b"exe");
        assert!(untar_xz(b"not xz", &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The real download (network, about 55 MB; 100 MB on Windows): `cargo test -p velt_toolchain zig -- --ignored`.
#[cfg(test)]
#[test]
#[ignore = "downloads zig from ziglang.org"]
fn the_pinned_zig_downloads_and_runs() {
    let home = std::env::temp_dir().join(format!("velt-zig-home-{}", std::process::id()));
    let exe = zig(&home).unwrap();
    let out = std::process::Command::new(&exe)
        .arg("version")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), VERSION);
    // The second time it is there already.
    assert_eq!(zig(&home).unwrap(), exe);
    let _ = std::fs::remove_dir_all(&home);
}
