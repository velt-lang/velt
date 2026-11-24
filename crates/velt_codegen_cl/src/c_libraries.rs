//! The C functions a program declares (`declare c`, docs/internals/design/c-ffi.md) under the
//! dev JIT: their addresses, from the libraries loaded into the host. On Unix that is every
//! image (`dlsym(RTLD_DEFAULT)`, what cranelift-jit's own lookup does); on Windows the
//! executable and the system DLLs a linked build imports std's C functions from, which the host
//! loads (cranelift-jit's lookup searches only the executable and the UCRT, so `GetLastError`
//! was missing, and bcrypt.dll isn't loaded unless someone loads it). A version checks every C
//! function first
//! ([`missing_c_functions`]), so a missing one is an error naming its declaration, not a panic
//! in cranelift-jit's `finalize_definitions`.

use velt_vir::vir;

/// The address of `name` in the libraries loaded into the host, or `None`.
pub(crate) fn loaded(name: &str) -> Option<*const u8> {
    platform::loaded(name).or_else(|| {
        let opened = OPENED.lock().unwrap_or_else(|e| e.into_inner());
        opened
            .iter()
            .find_map(|(_, handle)| platform::symbol(*handle, name))
    })
}

/// The libraries [`load_c_libraries`] opened: system name and handle. They stay open for the
/// life of the host, as the code calling into them does.
static OPENED: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());

/// Open the system libraries of a program's `declare c` functions in the dev host, so its C
/// functions resolve under the JIT as they do in a linked build: `sqlite3` is `libsqlite3.dylib`,
/// `libsqlite3.so` (or `libsqlite3.so.0`) or `sqlite3.dll`, found where the platform's loader
/// looks; `framework:Name` is the macOS framework. Each is opened once per host.
pub fn load_c_libraries(systems: &[String]) -> Result<(), String> {
    let mut opened = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    for system in systems {
        if opened.iter().any(|(s, _)| s == system) {
            continue;
        }
        let candidates = file_names(system);
        let handle = candidates.iter().find_map(|f| platform::open(f));
        match handle {
            Some(handle) => opened.push((system.clone(), handle)),
            None => {
                return Err(format!(
                    "C library `{system}` is not installed where `velt dev` can load it (tried {})",
                    candidates.join(", ")
                ))
            }
        }
    }
    Ok(())
}

/// Open the shared libraries `files` packages built from their C sources, with their symbols
/// global, as [`load_c_libraries`] does: each once per host, as it stays open. A rebuilt library
/// restarts the host.
pub fn load_c_library_files(files: &[std::path::PathBuf]) -> Result<(), String> {
    let mut opened = OPENED.lock().unwrap_or_else(|e| e.into_inner());
    for file in files {
        let name = file.display().to_string();
        if opened.iter().any(|(s, _)| *s == name) {
            continue;
        }
        let handle = platform::open(&name)
            .ok_or_else(|| format!("cannot load the C library `{name}` into `velt dev`"))?;
        opened.push((name, handle));
    }
    Ok(())
}

/// The file names the platform's loader may find system library `system` as.
fn file_names(system: &str) -> Vec<String> {
    if let Some(framework) = system.strip_prefix("framework:") {
        return vec![format!(
            "/System/Library/Frameworks/{framework}.framework/{framework}"
        )];
    }
    if cfg!(windows) {
        vec![format!("{system}.dll"), format!("lib{system}.dll")]
    } else if cfg!(target_os = "macos") {
        vec![format!("lib{system}.dylib")]
    } else {
        // The unversioned name is only a development link, and for glibc's libraries a linker
        // script `dlopen` refuses: `m` is `libm.so.6`.
        let versions = (0..10).map(|v| format!("lib{system}.so.{v}"));
        std::iter::once(format!("lib{system}.so"))
            .chain(versions)
            .collect()
    }
}

/// The address the host's own table gives `name` (`c_symbols`).
#[cfg(unix)]
fn own(name: &str) -> Option<*const u8> {
    crate::c_symbols::lookup(name)
}

/// The address the host's own table gives `name` (`win_symbols`).
#[cfg(windows)]
fn own(name: &str) -> Option<*const u8> {
    crate::win_symbols::lookup(name)
}

/// The error for the C functions of `program` the host can't find (`known`: the symbols the
/// version gets from elsewhere, the runtime's and earlier versions'), or `None` when it finds
/// every one.
pub(crate) fn missing_c_functions(
    program: &vir::Program,
    known: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let found = |symbol: &str| known(symbol) || own(symbol).is_some() || loaded(symbol).is_some();
    let mut missing: Vec<String> = (program.externs.iter())
        .filter_map(|e| e.c_decl.as_ref().map(|d| (e, d)))
        .filter(|(e, _)| !found(&e.symbol))
        .map(|(e, d)| format!("C function `{}` (declared at {d})", e.symbol))
        .collect();
    // C variables (`declare c("lib") let`): the JIT can't link a data symbol it doesn't find.
    missing.extend(
        (program.statics.iter())
            .filter(|s| s.c_global)
            .filter_map(|s| s.symbol.as_deref())
            .filter(|s| !found(s))
            .map(|s| format!("C variable `{s}`")),
    );
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "{} not found in the libraries loaded into `velt dev` (the C library and the packages' `native.c` libraries)",
        missing.join(", ")
    ))
}

#[cfg(unix)]
mod platform {
    use std::ffi::{c_char, c_int, c_void, CString};

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    }

    const RTLD_NOW: c_int = 2;
    #[cfg(target_os = "macos")]
    const RTLD_GLOBAL: c_int = 8;
    #[cfg(not(target_os = "macos"))]
    const RTLD_GLOBAL: c_int = 0x100;

    /// Open library `file` with its symbols global (`None`: not found).
    pub(super) fn open(file: &str) -> Option<usize> {
        let file = CString::new(file).ok()?;
        // SAFETY: `dlopen` reads the NUL-terminated name; the library's initializers run, as
        // they would in a linked program.
        let handle = unsafe { dlopen(file.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        (!handle.is_null()).then_some(handle as usize)
    }

    /// The address of `name` in the library `handle` opened.
    pub(super) fn symbol(handle: usize, name: &str) -> Option<*const u8> {
        let symbol = CString::new(name).ok()?;
        // SAFETY: `handle` came from `dlopen` and stays open; the address is not called here.
        let address = unsafe { dlsym(handle as *mut c_void, symbol.as_ptr()) };
        (!address.is_null()).then_some(address as *const u8)
    }

    pub(super) fn loaded(name: &str) -> Option<*const u8> {
        // `RTLD_DEFAULT`: every image loaded into the process, in load order.
        #[cfg(target_os = "macos")]
        let default = -2isize as *mut c_void;
        #[cfg(not(target_os = "macos"))]
        let default = std::ptr::null_mut();
        let symbol = CString::new(name).ok()?;
        // SAFETY: `dlsym` only reads the NUL-terminated name; the address is not called here.
        let address = unsafe { dlsym(default, symbol.as_ptr()) };
        (!address.is_null()).then_some(address as *const u8)
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::{c_char, c_void, CString};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn LoadLibraryExA(name: *const c_char, file: *mut c_void, flags: u32) -> *mut c_void;
    }

    /// `LoadLibraryExA`: search only System32, never the current or the executable's directory.
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;

    /// The system DLLs a linked debug build gets std's own `declare c("c")` functions from
    /// (velt_link `system.rs`'s Windows `shared_native_libs`, whose import libraries name them):
    /// the C library (`msvcrt.lib` brings the UCRT), kernel32, advapi32 (the time zones of
    /// std/time/localtime_win32.velt), Winsock, ntdll and the synchronization library
    /// (std/lang/hooks/reactor_afd.velt), and bcrypt (std/lang/random/os_win32.velt's
    /// `BCryptGenRandom`). The host doesn't have each of them loaded on its own (bcrypt.dll only
    /// came with the runtime's former TLS library), so it loads them, from System32, once.
    const SYSTEM_DLLS: [&[u8]; 7] = [
        b"ucrtbase.dll\0",
        b"kernel32.dll\0",
        b"advapi32.dll\0",
        b"ws2_32.dll\0",
        b"ntdll.dll\0",
        b"api-ms-win-core-synch-l1-2-0.dll\0",
        b"bcrypt.dll\0",
    ];

    /// The handles of [`SYSTEM_DLLS`] (0: not loaded), loaded on first use and kept for the
    /// life of the host.
    fn system_dlls() -> &'static [usize] {
        static HANDLES: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
        HANDLES.get_or_init(|| {
            (SYSTEM_DLLS.iter())
                .map(|name| {
                    // SAFETY: the name is NUL-terminated; a system DLL's initializers run, as
                    // they do in a linked program that imports it.
                    unsafe {
                        LoadLibraryExA(
                            name.as_ptr().cast(),
                            std::ptr::null_mut(),
                            LOAD_LIBRARY_SEARCH_SYSTEM32,
                        ) as usize
                    }
                })
                .collect()
        })
    }

    /// Load library `file` (`None`: not found).
    pub(super) fn open(file: &str) -> Option<usize> {
        let file = CString::new(file).ok()?;
        // SAFETY: `LoadLibraryA` reads the NUL-terminated name; the DLL's initializers run, as
        // they would in a linked program.
        let module = unsafe { LoadLibraryA(file.as_ptr()) };
        (!module.is_null()).then_some(module as usize)
    }

    /// The address of `name` in the module `handle` loaded.
    pub(super) fn symbol(handle: usize, name: &str) -> Option<*const u8> {
        let symbol = CString::new(name).ok()?;
        // SAFETY: `handle` came from `LoadLibraryA` and stays loaded; the address is not called.
        let address = unsafe { GetProcAddress(handle as *mut c_void, symbol.as_ptr()) };
        (!address.is_null()).then_some(address as *const u8)
    }

    pub(super) fn loaded(name: &str) -> Option<*const u8> {
        let symbol = CString::new(name).ok()?;
        // The executable first, then the system DLLs a linked build imports std's C functions
        // from.
        // SAFETY: NULL names the executable, which is loaded.
        let exe = unsafe { GetModuleHandleA(std::ptr::null()) } as usize;
        std::iter::once(exe)
            .chain(system_dlls().iter().copied())
            .filter(|&module| module != 0)
            .find_map(|module| {
                // SAFETY: `module` is loaded for the life of the host; the name is
                // NUL-terminated, and an address found is not called here.
                let address = unsafe { GetProcAddress(module as *mut c_void, symbol.as_ptr()) };
                (!address.is_null()).then_some(address as *const u8)
            })
    }

    /// The system DLLs [`loaded`] searches, by name, with whether each loaded.
    #[cfg(test)]
    pub(super) fn system_dll_names() -> Vec<(String, bool)> {
        (SYSTEM_DLLS.iter().zip(system_dlls()))
            .map(|(name, &h)| {
                let name = String::from_utf8_lossy(&name[..name.len() - 1]).into_owned();
                (name, h != 0)
            })
            .collect()
    }
}

#[cfg(all(test, windows))]
#[path = "c_libraries_tests.rs"]
mod tests;
