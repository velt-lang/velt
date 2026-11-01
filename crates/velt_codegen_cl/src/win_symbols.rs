//! The Windows functions generated code may call that `cranelift-jit`'s lookup through the loaded
//! CRT modules does not find, by address, for the JIT: kernel32's thread-local index functions,
//! which a `ThreadLocal`'s functions use where the JIT has no thread-local storage (tls.rs).

use std::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    fn TlsAlloc() -> u32;
    fn TlsFree(index: u32) -> i32;
    fn TlsGetValue(index: u32) -> *mut c_void;
    fn TlsSetValue(index: u32, value: *mut c_void) -> i32;
}

/// The address of Windows function `name`, if generated code may call it and the default lookup
/// does not find it.
pub(crate) fn lookup(name: &str) -> Option<*const u8> {
    let address = match name {
        "TlsAlloc" => TlsAlloc as *const u8,
        "TlsFree" => TlsFree as *const u8,
        "TlsGetValue" => TlsGetValue as *const u8,
        "TlsSetValue" => TlsSetValue as *const u8,
        _ => return None,
    };
    Some(address)
}
