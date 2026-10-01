//! Linking against the runtime as a shared library (`velt_rt_shared`: `libvelt_rt_shared.so`,
//! `libvelt_rt_shared.dylib`, `velt_rt_shared.dll` + its import library `velt_rt_shared.dll.lib`).
//! Debug builds use it: the static runtime holds tokio, hyper, rustls, SQLite, … and linking it
//! takes seconds, while resolving against a shared library's symbol table takes milliseconds.
//!
//! The executable finds the library through an rpath to the directory it was found in (Linux,
//! macOS), or a copy next to the executable (Windows, which has no rpath). The program's own
//! `main` comes from `velt_codegen_cl::emit_entry_object` (the library has none).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::TargetOs;

/// File name the linker is given for the shared runtime of `os`: the library itself on Unix, its
/// import library on Windows.
pub fn shared_runtime_lib_name(os: TargetOs) -> &'static str {
    match os {
        TargetOs::Windows => "velt_rt_shared.dll.lib",
        TargetOs::MacOs => "libvelt_rt_shared.dylib",
        TargetOs::Linux => "libvelt_rt_shared.so",
    }
}

/// The DLL that must sit next to a Windows executable linked against the shared runtime.
const WINDOWS_DLL: &str = "velt_rt_shared.dll";

/// Whether `runtime_lib` names the shared runtime (so [`crate::link`] links dynamically).
pub(crate) fn is_shared(runtime_lib: &Path, os: TargetOs) -> bool {
    runtime_lib
        .file_name()
        .is_some_and(|n| n == shared_runtime_lib_name(os))
}

/// Linker arguments that link the shared runtime at `lib` (a Unix `cc` driver): by `-l` name so
/// the executable records the library's name, not the build machine's path, plus an rpath to
/// its directory.
pub(crate) fn unix_args(lib: &Path) -> Vec<OsString> {
    let dir = absolute_dir(lib);
    let mut search = OsString::from("-L");
    search.push(&dir);
    let mut rpath = OsString::from("-Wl,-rpath,");
    rpath.push(&dir);
    vec![search, "-lvelt_rt_shared".into(), rpath]
}

fn absolute_dir(lib: &Path) -> PathBuf {
    let lib = std::fs::canonicalize(lib).unwrap_or_else(|_| lib.to_path_buf());
    lib.parent().map_or_else(PathBuf::new, Path::to_path_buf)
}

/// Windows: copy `velt_rt_shared.dll` (and its `.pdb`, for debuggers) from beside the import
/// library to beside `output`, unless an identical-looking copy (same size and modification
/// time) is already there.
pub(crate) fn place_dll(import_lib: &Path, output: &Path) -> Result<(), String> {
    let (Some(from_dir), Some(to_dir)) = (import_lib.parent(), output.parent()) else {
        return Ok(());
    };
    let to_dir = if to_dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        to_dir
    };
    for name in [WINDOWS_DLL, "velt_rt_shared.pdb"] {
        let from = from_dir.join(name);
        let to = to_dir.join(name);
        if from == to || !from.is_file() || same_file_stamp(&from, &to) {
            continue;
        }
        copy_atomically(&from, &to).map_err(|e| {
            format!(
                "cannot copy the runtime library {} next to the executable: {e}",
                from.display()
            )
        })?;
    }
    Ok(())
}

/// Copy through a temporary file and a rename, so parallel builds into one directory never see
/// a half-written DLL; losing the race to an identical copy (or to a copy in use by a running
/// program) is fine as long as the result is up to date.
fn copy_atomically(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tmp_name = to.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = to.with_file_name(tmp_name);
    std::fs::copy(from, &tmp)?;
    let renamed = std::fs::rename(&tmp, to);
    let _ = std::fs::remove_file(&tmp);
    match renamed {
        Err(_) if same_file_stamp(from, to) => Ok(()),
        other => other,
    }
}

fn same_file_stamp(a: &Path, b: &Path) -> bool {
    let stamp = |p: &Path| {
        let m = std::fs::metadata(p).ok()?;
        Some((m.len(), m.modified().ok()?))
    };
    // `fs::copy` keeps the modification time on Windows, so an up-to-date copy compares equal.
    matches!((stamp(a), stamp(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_detection() {
        assert!(is_shared(
            Path::new("/t/debug/libvelt_rt_shared.so"),
            TargetOs::Linux
        ));
        assert!(!is_shared(
            Path::new("/t/debug/libvelt_rt.a"),
            TargetOs::Linux
        ));
        assert!(is_shared(
            Path::new("C:/t/velt_rt_shared.dll.lib"),
            TargetOs::Windows
        ));
        assert!(!is_shared(Path::new("velt_rt.lib"), TargetOs::Windows));
        assert_eq!(
            shared_runtime_lib_name(TargetOs::MacOs),
            "libvelt_rt_shared.dylib"
        );
    }

    #[test]
    fn unix_args_use_the_library_name_and_an_rpath() {
        let a: Vec<String> = unix_args(Path::new("/opt/velt/lib/libvelt_rt_shared.so"))
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            a,
            [
                "-L/opt/velt/lib",
                "-lvelt_rt_shared",
                "-Wl,-rpath,/opt/velt/lib"
            ]
        );
    }

    #[test]
    fn dll_is_copied_once() {
        let tmp = std::env::temp_dir().join(format!("velt_link_dll_{}", std::process::id()));
        let (rt, out) = (tmp.join("rt"), tmp.join("out"));
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(rt.join(WINDOWS_DLL), b"dll").unwrap();
        let import = rt.join("velt_rt_shared.dll.lib");
        place_dll(&import, &out.join("app.exe")).unwrap();
        assert_eq!(std::fs::read(out.join(WINDOWS_DLL)).unwrap(), b"dll");
        // No .pdb beside the DLL: nothing to copy, no error.
        assert!(!out.join("velt_rt_shared.pdb").exists());
        place_dll(&import, &out.join("app.exe")).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
