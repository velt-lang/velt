//! Building a package's native crate into a bundle: `cargo rustc --release --target <triple>`,
//! then reading its exports and, on Linux and macOS, prelinking the staticlib into one relocatable
//! object whose only global symbols are the package's exports. A Rust staticlib carries its own
//! copy of std; prelinked and localized, it cannot clash with the runtime's copy (or another
//! package's) in a statically linked executable. Windows has no partial link, so release builds
//! there use the DLL too (native_abi.md "Linking").
//!
//! Tools (overridable for cross builds): `$VELT_CARGO` (default `cargo`), `$VELT_NATIVE_LD`
//! (default `ld`; GNU ld on Linux, ld64 on macOS) and `$VELT_NATIVE_OBJCOPY` (default `objcopy`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::manifest::Manifest;
use crate::native::{
    exports, init_symbol, library_files, library_name, NativeMeta, META_FILE, NATIVE_ABI,
};

/// What to build.
#[derive(Clone, Copy, Debug)]
pub struct BuildRequest<'a> {
    /// Package root (contains package.vlt and the `native` crate directory).
    pub root: &'a Path,
    /// Its manifest (must have `native`).
    pub manifest: &'a Manifest,
    /// Target triple.
    pub target: &'a str,
    /// The bundle directory to write (replaced).
    pub out: &'a Path,
    /// Cargo's `--target-dir` (kept between builds so rebuilds are incremental).
    pub cargo_target_dir: &'a Path,
    /// Build exactly the crate's `Cargo.lock` (`cargo --locked`): for packages from a registry.
    pub locked: bool,
}

fn tool(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The cargo program (`$VELT_CARGO` or `cargo`).
pub fn cargo() -> String {
    tool("VELT_CARGO", "cargo")
}

/// Whether cargo can be run here.
pub fn cargo_available() -> bool {
    Command::new(cargo())
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Build the bundle for `req.target` into `req.out` and return its metadata. Cargo's progress goes
/// to stderr. When the bundle is newer than cargo's outputs, only cargo runs (a no-op build).
pub fn build(req: BuildRequest) -> Result<NativeMeta, String> {
    let name = &req.manifest.package.name;
    let native = req
        .manifest
        .native
        .as_ref()
        .ok_or_else(|| format!("package `{name}` has no `native` in package.vlt"))?;
    let crate_manifest = req.root.join(&native.path).join("Cargo.toml");
    if !crate_manifest.is_file() {
        return Err(format!(
            "package `{name}`: `{}` does not exist (`native.path` is \"{}\")",
            crate_manifest.display(),
            native.path
        ));
    }
    let artifacts = run_cargo(
        &crate_manifest,
        req.target,
        req.cargo_target_dir,
        req.locked,
    )?;
    let shared = artifacts.shared.ok_or_else(|| {
        format!("package `{name}`: the crate builds no `cdylib`; set `crate-type = [\"cdylib\", \"staticlib\"]` in [lib]")
    })?;
    check_library_name(name, &shared, req.target)?;
    let windows = req.target.contains("windows");
    if !windows && artifacts.staticlib.is_none() {
        return Err(format!(
            "package `{name}`: the crate builds no `staticlib`; set `crate-type = [\"cdylib\", \"staticlib\"]` in [lib]"
        ));
    }

    let newest = [Some(&shared), artifacts.staticlib.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max();
    // One process at a time checks and writes the bundle: programs built at once (goldens, tests)
    // may all find it out of date, and one must not replace it while another reads it.
    let _lock = lock_bundle(req.out)?;
    if let Some(meta) = current_bundle(&req, newest) {
        return Ok(meta);
    }

    let bytes =
        std::fs::read(&shared).map_err(|e| format!("cannot read `{}`: {e}", shared.display()))?;
    let exports = exports::read(&bytes, name)
        .map_err(|e| format!("package `{name}` ({}): {e}", shared.display()))?;

    let tmp = req.out.with_extension("tmp");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)
            .map_err(|e| format!("cannot clean `{}`: {e}", tmp.display()))?;
    }
    let mkdir = |d: &Path| {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create `{}`: {e}", d.display()))
    };
    mkdir(&tmp.join("shared"))?;
    let copy = |from: &Path, to: &Path| {
        std::fs::copy(from, to)
            .map(drop)
            .map_err(|e| format!("cannot copy `{}`: {e}", from.display()))
    };

    let lib = name.replace('-', "_");
    let (shared_rel, import_rel) = if windows {
        // A DLL's name is recorded in its import library: keep cargo's names.
        let file = file_name(&shared);
        copy(&shared, &tmp.join("shared").join(&file))?;
        let import = artifacts.import_lib.ok_or_else(|| {
            format!("package `{name}`: cargo produced no import library for `{file}`")
        })?;
        let import_file = file_name(&import);
        copy(&import, &tmp.join("shared").join(&import_file))?;
        (
            format!("shared/{file}"),
            Some(format!("shared/{import_file}")),
        )
    } else {
        let (file, _) = library_files(name, req.target);
        let dest = tmp.join("shared").join(&file);
        copy(&shared, &dest)?;
        if req.target.contains("apple") {
            run(Command::new("install_name_tool")
                .arg("-id")
                .arg(format!("@rpath/{file}"))
                .arg(&dest))?;
        }
        (format!("shared/{file}"), None)
    };

    let static_rel = match &artifacts.staticlib {
        Some(staticlib) if !windows => {
            mkdir(&tmp.join("static"))?;
            let rel = format!("static/{lib}.o");
            let mut keep: Vec<String> = exports.keys().cloned().collect();
            keep.push(init_symbol(name));
            prelink(staticlib, &keep, req.target, &tmp.join(&rel))?;
            Some(rel)
        }
        _ => None,
    };

    // The system libraries the object needs (a shared library brings its own).
    let system_libs = match &static_rel {
        Some(_) => Some(read_system_libs(&artifacts.system_libs, name)?),
        None => None,
    };
    let meta = NativeMeta {
        package: name.clone(),
        version: req.manifest.package.version.clone(),
        target: req.target.to_string(),
        abi: NATIVE_ABI,
        shared: shared_rel,
        import_lib: import_rel,
        static_obj: static_rel,
        system_libs,
        exports,
    };
    crate::json_file::write_atomic(&tmp.join(META_FILE), &meta.to_json())?;
    if req.out.exists() {
        std::fs::remove_dir_all(req.out)
            .map_err(|e| format!("cannot clean `{}`: {e}", req.out.display()))?;
    }
    if let Some(parent) = req.out.parent() {
        mkdir(parent)?;
    }
    std::fs::rename(&tmp, req.out)
        .map_err(|e| format!("cannot write `{}`: {e}", req.out.display()))?;
    Ok(meta)
}

/// An exclusive lock on the bundle directory `out` (the file `<out>.lock` beside it), released
/// when the result is dropped.
fn lock_bundle(out: &Path) -> Result<std::fs::File, String> {
    let path = out.with_extension("lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create `{}`: {e}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open `{}`: {e}", path.display()))?;
    file.lock()
        .map_err(|e| format!("cannot lock `{}`: {e}", path.display()))?;
    Ok(file)
}

/// The bundle at `req.out` when it is this version's for this target, newer than cargo's outputs
/// (`newest`) and whole (every file its `native.json` names is there): nothing to write.
fn current_bundle(req: &BuildRequest, newest: Option<std::time::SystemTime>) -> Option<NativeMeta> {
    let meta = NativeMeta::read(req.out).ok()?;
    let built = std::fs::metadata(req.out.join(META_FILE))
        .and_then(|m| m.modified())
        .ok()?;
    let current = meta.version == req.manifest.package.version && meta.target == req.target;
    let whole = [
        Some(&meta.shared),
        meta.import_lib.as_ref(),
        meta.static_obj.as_ref(),
    ]
    .into_iter()
    .flatten()
    .all(|f| req.out.join(f).is_file());
    // A bundle from before velt recorded the object's system libraries is built again.
    let libs = meta.static_obj.is_none() || meta.system_libs.is_some();
    (current && whole && libs && built >= newest?).then_some(meta)
}

/// The system libraries rustc wrote for the staticlib (`--print native-static-libs=<file>`), as
/// linker arguments.
fn read_system_libs(file: &Path, package: &str) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| {
        format!(
            "package `{package}`: cannot read the native library's system libraries `{}`: {e}",
            file.display()
        )
    })?;
    Ok(text.split_whitespace().map(String::from).collect())
}

/// The crate's library must be `velt_native_<pkg>` (`[lib] name`): its file name is what links,
/// loads and (on Windows) sits next to the executable, so two packages' libraries must never share
/// one.
fn check_library_name(package: &str, shared: &Path, target: &str) -> Result<(), String> {
    let expected = library_name(package);
    let (file, _) = library_files(package, target);
    if file_name(shared) == file {
        return Ok(());
    }
    Err(format!(
        "package `{package}`: the native crate's library is `{}`, but must be named `{expected}` (add `name = \"{expected}\"` to the crate's [lib] table)",
        file_name(shared)
    ))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Default, Debug)]
struct Artifacts {
    shared: Option<PathBuf>,
    import_lib: Option<PathBuf>,
    staticlib: Option<PathBuf>,
    /// Where rustc wrote the staticlib's `native-static-libs`.
    system_libs: PathBuf,
}

fn run_cargo(
    crate_manifest: &Path,
    target: &str,
    target_dir: &Path,
    locked: bool,
) -> Result<Artifacts, String> {
    let cargo = cargo();
    if locked && !crate_manifest.with_file_name("Cargo.lock").is_file() {
        return Err(format!(
            "`{}` has no Cargo.lock to build exactly (the package must publish its crate's lockfile)",
            crate_manifest.display()
        ));
    }
    // `cargo rustc` to have rustc write the system libraries the staticlib needs to a file (the
    // same argument every time, so a build with nothing new is still a no-op).
    let system_libs = target_dir
        .join(target)
        .join("release")
        .join("velt-native-static-libs.txt");
    let mut print = std::ffi::OsString::from("native-static-libs=");
    print.push(&system_libs);
    let out = Command::new(&cargo)
        .args(locked.then_some("--locked"))
        .args([
            "rustc",
            "--release",
            "--lib",
            "--message-format=json-render-diagnostics",
        ])
        .arg("--target")
        .arg(target)
        .arg("--manifest-path")
        .arg(crate_manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--")
        .arg("--print")
        .arg(print)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run `{cargo}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{cargo} rustc` failed for `{}` ({target})",
            crate_manifest.display()
        ));
    }
    let wanted = std::fs::canonicalize(crate_manifest).unwrap_or(crate_manifest.to_path_buf());
    let mut found = Artifacts {
        system_libs,
        ..Artifacts::default()
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg["reason"] != "compiler-artifact" {
            continue;
        }
        let manifest = msg["manifest_path"].as_str().map(PathBuf::from);
        let ours = manifest
            .map(|m| std::fs::canonicalize(&m).unwrap_or(m))
            .is_some_and(|m| m == wanted);
        if !ours {
            continue;
        }
        for file in msg["filenames"].as_array().into_iter().flatten() {
            let Some(f) = file.as_str() else { continue };
            let path = PathBuf::from(f);
            if f.ends_with(".dll.lib") {
                found.import_lib = Some(path);
            } else if f.ends_with(".so") || f.ends_with(".dylib") || f.ends_with(".dll") {
                found.shared = Some(path);
            } else if f.ends_with(".a") || (f.ends_with(".lib") && target.contains("windows")) {
                found.staticlib = Some(path);
            }
        }
    }
    Ok(found)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {shown}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{shown} failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `-platform_version` for `ld -r` on Apple targets: Xcode 15's linker refuses to link without it.
/// The object gets the runtime's minimum macOS (11.0 on arm64, 10.12 on x86_64), never newer than
/// the executable's deployment target, so the final link neither warns nor raises the minimum.
fn apple_platform_version(target: &str) -> [&'static str; 4] {
    let min = if target.starts_with("x86_64") {
        "10.12"
    } else {
        "11.0"
    };
    ["-platform_version", "macos", min, min]
}

/// Prelink `staticlib` into `out`: the members `keep` needs, one object, only `keep` global.
fn prelink(staticlib: &Path, keep: &[String], target: &str, out: &Path) -> Result<(), String> {
    let ld = tool("VELT_NATIVE_LD", "ld");
    let dir = out.parent().expect("ICE: bundle file has a directory");
    let list = dir.join("keep.txt");
    let apple = target.contains("apple");
    let names: Vec<String> = keep
        .iter()
        .map(|s| if apple { format!("_{s}") } else { s.clone() })
        .collect();
    std::fs::write(&list, names.join("\n") + "\n")
        .map_err(|e| format!("cannot write `{}`: {e}", list.display()))?;
    let undefined = names.iter().flat_map(|n| ["-u".to_string(), n.clone()]);
    let result = if apple {
        let arch = match target.split('-').next() {
            Some("aarch64") => "arm64",
            Some(a) => a,
            None => "x86_64",
        };
        run(Command::new(&ld)
            .args(["-r", "-arch", arch])
            .args(apple_platform_version(target))
            .arg("-exported_symbols_list")
            .arg(&list)
            .args(undefined)
            .arg("-o")
            .arg(out)
            .arg(staticlib))
    } else {
        // Section groups (COMDATs) would let the runtime's copy of a group replace ours (or the
        // reverse) and leave references to now-local symbols dangling: make them plain sections.
        let full = dir.join("full.o");
        run(Command::new(&ld)
            .args(["-r", "--force-group-allocation", "-o"])
            .arg(&full)
            .args(undefined)
            .arg(staticlib))
        .and_then(|()| {
            let objcopy = tool("VELT_NATIVE_OBJCOPY", "objcopy");
            let r = run(Command::new(objcopy)
                .arg("--strip-debug")
                .arg(format!("--keep-global-symbols={}", list.display()))
                .arg(&full)
                .arg(out));
            let _ = std::fs::remove_file(&full);
            r
        })
    };
    let _ = std::fs::remove_file(&list);
    result.map_err(|e| format!("cannot prelink the native library for {target}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two processes building one package's bundle take turns: the lock beside the bundle is
    /// exclusive until its holder drops it.
    #[test]
    fn the_bundle_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("aarch64-apple-darwin");
        let held = lock_bundle(&out).unwrap();
        let other = std::fs::OpenOptions::new()
            .write(true)
            .open(out.with_extension("lock"))
            .unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(other.try_lock().is_ok());
    }

    #[test]
    fn apple_prelinks_name_the_runtimes_minimum_macos() {
        assert_eq!(
            apple_platform_version("aarch64-apple-darwin"),
            ["-platform_version", "macos", "11.0", "11.0"]
        );
        assert_eq!(
            apple_platform_version("x86_64-apple-darwin"),
            ["-platform_version", "macos", "10.12", "10.12"]
        );
    }

    #[test]
    fn library_names() {
        let ok = |file: &str, target: &str| check_library_name("pg-lite", Path::new(file), target);
        ok("/t/libvelt_native_pg_lite.so", "x86_64-unknown-linux-gnu").unwrap();
        ok("/t/libvelt_native_pg_lite.dylib", "aarch64-apple-darwin").unwrap();
        ok("/t/velt_native_pg_lite.dll", "x86_64-pc-windows-msvc").unwrap();
        let e = ok("/t/native.dll", "x86_64-pc-windows-msvc").unwrap_err();
        assert!(e.contains("name = \"velt_native_pg_lite\""), "{e}");
        assert!(ok("/t/libnative.so", "x86_64-unknown-linux-gnu").is_err());
    }
}
