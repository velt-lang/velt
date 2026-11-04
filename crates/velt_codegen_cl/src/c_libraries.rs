//! The C functions a program declares (`declare c`, docs/internals/design/c-ffi.md) under the
//! dev JIT: their addresses, from the libraries loaded into the host. On Unix that is every
//! image (`dlsym(RTLD_DEFAULT)`, what cranelift-jit's own lookup does); on Windows the
//! executable, the UCRT and kernel32 (cranelift-jit's lookup searches only the first two, so
//! `GetLastError` was missing). A version checks every C function first
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
    let missing: Vec<String> = (program.externs.iter())
        .filter_map(|e| e.c_decl.as_ref().map(|d| (e, d)))
        .filter(|(e, _)| !known(&e.symbol) && own(&e.symbol).is_none())
        .filter(|(e, _)| loaded(&e.symbol).is_none())
        .map(|(e, d)| format!("`{}` (declared at {d})", e.symbol))
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "C function {} not found in the libraries loaded into `velt dev` (the C library and the packages' `native.c` libraries)",
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
        let modules: [&[u8]; 3] = [b"\0", b"ucrtbase.dll\0", b"kernel32.dll\0"];
        modules.iter().find_map(|m| {
            // SAFETY: the names are NUL-terminated; a module that isn't loaded gives NULL, and
            // an address found is not called here.
            let address = unsafe {
                let module = match m.len() {
                    1 => GetModuleHandleA(std::ptr::null()),
                    _ => GetModuleHandleA(m.as_ptr().cast()),
                };
                if module.is_null() {
                    return None;
                }
                GetProcAddress(module, symbol.as_ptr())
            };
            (!address.is_null()).then_some(address as *const u8)
        })
    }
}
