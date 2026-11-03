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
    platform::loaded(name)
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
        "C function {} not found in the libraries loaded into `velt dev` (the C library's; libraries of packages per target come in a later step of #2187)",
        missing.join(", ")
    ))
}

#[cfg(unix)]
mod platform {
    use std::ffi::{c_char, c_void, CString};

    extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
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
