//! Native libraries in the `velt dev` JIT host (docs/internals/contracts/native_abi.md "velt dev"):
//! each package's shared library is loaded into the host (`dlopen` / `LoadLibraryW`, nothing is
//! linked) and its exports, init function included, are handed to the JIT like the runtime's
//! symbols. Libraries are loaded once and never unloaded, so the code pointers they hold stay
//! valid across hot swaps (hot-reload rule, rt_abi_async.md §13.5); a change to a library built
//! from source restarts the host instead of swapping ([`Fingerprint`]).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vpm::native::NativeOrigin;
use vpm::PackageGraph;

/// The symbols of every native library in `graph`, loaded into this process.
pub fn jit_symbols(graph: Option<&PackageGraph>) -> Result<Vec<(String, usize)>, String> {
    let mut out = vec![];
    for (pkg, lib) in graph.into_iter().flat_map(|g| g.natives()) {
        if cfg!(target_env = "musl") {
            return Err(format!(
                "package `{}` has native code, which this statically linked velt cannot load into \
                 its JIT host; use `velt dev --exe`",
                pkg.name
            ));
        }
        let path = lib.shared_lib();
        let handle = sys::open(&path).map_err(|e| {
            format!(
                "cannot load the native library of package `{}` ({}): {e}",
                pkg.name,
                path.display()
            )
        })?;
        let names = lib.meta.exports.keys().cloned().chain([lib.init_symbol()]);
        for name in names {
            let addr = sys::symbol(handle, &name).ok_or_else(|| {
                format!(
                    "the native library of package `{}` ({}) does not export `{name}`",
                    pkg.name,
                    path.display()
                )
            })?;
            out.push((name, addr));
        }
    }
    Ok(out)
}

/// When the sources of the native libraries built from source were last changed: a different
/// fingerprint after a rebuild means the running host has stale native code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint(Vec<(PathBuf, Option<SystemTime>)>);

impl Fingerprint {
    /// The fingerprint of `graph`'s native libraries built from source.
    pub fn of(graph: Option<&PackageGraph>) -> Fingerprint {
        Fingerprint(
            source_files(graph)
                .into_iter()
                .map(|f| {
                    let t = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
                    (f, t)
                })
                .collect(),
        )
    }
}

/// The crate sources of native libraries built from source (path packages): the files `velt dev`
/// watches besides the Velt sources.
pub fn source_files(graph: Option<&PackageGraph>) -> Vec<PathBuf> {
    let mut out = vec![];
    for (pkg, lib) in graph.into_iter().flat_map(|g| g.natives()) {
        if lib.origin != NativeOrigin::BuiltFromSource {
            continue;
        }
        let Ok(manifest) = vpm::Manifest::from_dir(&pkg.root) else {
            continue;
        };
        if let Some(native) = manifest.native {
            collect(&pkg.root.join(native.path), &mut out);
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        if path.is_dir() {
            if name != "target" && name != ".git" {
                collect(&path, out);
            }
        } else {
            out.push(path);
        }
    }
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// A loaded library (never closed).
    #[derive(Clone, Copy)]
    pub struct Handle(*mut libc::c_void);

    pub fn open(path: &Path) -> Result<Handle, String> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL in path")?;
        // SAFETY: a valid C string; the library is never closed.
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            // SAFETY: dlerror returns a thread-local message or null.
            let msg = unsafe { libc::dlerror() };
            return Err(if msg.is_null() {
                "dlopen failed".into()
            } else {
                // SAFETY: non-null C string from dlerror.
                unsafe { CStr::from_ptr(msg) }
                    .to_string_lossy()
                    .into_owned()
            });
        }
        Ok(Handle(h))
    }

    pub fn symbol(h: Handle, name: &str) -> Option<usize> {
        let c = CString::new(name).ok()?;
        // SAFETY: a handle from dlopen and a valid C string.
        let p = unsafe { libc::dlsym(h.0, c.as_ptr()) };
        (!p.is_null()).then_some(p as usize)
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    /// A loaded library (never freed).
    #[derive(Clone, Copy)]
    pub struct Handle(windows_sys::Win32::Foundation::HMODULE);

    pub fn open(path: &Path) -> Result<Handle, String> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: a NUL-terminated wide path; the library is never freed.
        let h = unsafe { LoadLibraryW(wide.as_ptr()) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Handle(h))
    }

    pub fn symbol(h: Handle, name: &str) -> Option<usize> {
        let c = std::ffi::CString::new(name).ok()?;
        // SAFETY: a module handle from LoadLibraryW and a NUL-terminated name.
        let p = unsafe { GetProcAddress(h.0, c.as_ptr() as *const u8) };
        p.map(|f| f as usize)
    }
}
