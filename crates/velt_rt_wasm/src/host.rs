//! What host functions (https://veltlang.com/docs/reference/host-functions/) need of the runtime on WebAssembly:
//! - `HostRef`'s release and clone (std/host/rt.velt), which reach the JS glue's table of values
//!   through its imports `velt.host_release` and `velt.host_clone` (wasm32-unknown-unknown;
//!   under WASI no host function can make a `HostRef`, calling one is a compile error);
//! - the exports the glue (crates/velt_rt_wasm/js/velt_web.mjs) calls to convert the values that
//!   cross (velt_vir's `vir::HostImport` docs): the bytes of a string argument, a buffer for a
//!   result, and the string or byte array made of it. velt_link exports them from the module.

use crate::bytes::VeltBytes;
use crate::str::VeltStr;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// Forget the value in slot `handle` of the glue's table.
        pub fn host_release(handle: u32);
        /// A new slot holding the value of slot `handle`.
        pub fn host_clone(handle: u32) -> u32;
    }
}

/// `HostRef[Symbol.dispose]()`: the host forgets the value in slot `handle`.
#[no_mangle]
pub extern "C" fn velt_rt_host_release(handle: u32) {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: a plain import taking a number.
    unsafe {
        imports::host_release(handle)
    };
    let _ = handle;
}

/// `HostRef.clone()`: a new slot holding the value of slot `handle` (0 without a JS host, where
/// no `HostRef` holds a slot).
#[no_mangle]
pub extern "C" fn velt_rt_host_clone(handle: u32) -> u32 {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: a plain import taking and returning a number.
    return unsafe { imports::host_clone(handle) };
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        let _ = handle;
        0
    }
}

/// The address of the bytes of string `s` (a host function's argument; they stay valid for the
/// call).
///
/// # Safety
/// `s` points to a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_ptr(s: *const VeltStr) -> *const u8 {
    (*s).as_bytes().as_ptr()
}

/// The length in bytes of string `s`.
///
/// # Safety
/// `s` points to a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_len(s: *const VeltStr) -> usize {
    (*s).as_bytes().len()
}

/// A buffer of `len` bytes for the glue to write a result into, which
/// [`velt_host_str_new`] or [`velt_host_bytes_new`] then takes.
#[no_mangle]
pub extern "C" fn velt_host_alloc(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

/// The buffer of `len` bytes at `p` that [`velt_host_alloc`] made, back as a vector.
///
/// # Safety
/// `p` and `len` come from one call of [`velt_host_alloc`], and nothing took the buffer yet.
unsafe fn take(p: *mut u8, len: usize) -> Vec<u8> {
    Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)).into_vec()
}

/// Write to `out` the string of the UTF-8 text in the buffer `p` (`len` bytes, from
/// [`velt_host_alloc`]), and free the buffer: a host function's string result. Bytes that are
/// not UTF-8 become U+FFFD.
///
/// # Safety
/// As for [`take`]; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_host_str_new(out: *mut VeltStr, p: *mut u8, len: usize) {
    let bytes = take(p, len);
    let s = match std::str::from_utf8(&bytes) {
        Ok(text) => VeltStr::from_text(text),
        Err(_) => VeltStr::from_text(&String::from_utf8_lossy(&bytes)),
    };
    out.write(s);
}

/// Write to `out` the byte array owning the buffer `p` (`len` bytes, from [`velt_host_alloc`]):
/// a host function's `u8[]` result.
///
/// # Safety
/// As for [`take`]; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_host_bytes_new(out: *mut VeltBytes, p: *mut u8, len: usize) {
    out.write(VeltBytes::from_vec(take(p, len)));
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn filled(text: &[u8]) -> *mut u8 {
        let p = velt_host_alloc(text.len());
        std::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
        p
    }

    #[test]
    fn strings_cross_both_ways() {
        for text in ["", "short", "héllo, a string longer than an inline one"] {
            unsafe {
                let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
                velt_host_str_new(out.as_mut_ptr(), filled(text.as_bytes()), text.len());
                let mut s = out.assume_init();
                let bytes =
                    std::slice::from_raw_parts(velt_host_str_ptr(&s), velt_host_str_len(&s));
                assert_eq!(bytes, text.as_bytes());
                s.release();
            }
        }
    }

    #[test]
    fn bytes_are_adopted_and_bad_text_is_replaced() {
        unsafe {
            let mut out = std::mem::MaybeUninit::<VeltBytes>::uninit();
            velt_host_bytes_new(out.as_mut_ptr(), filled(&[1, 2, 255]), 3);
            let mut b = out.assume_init();
            assert_eq!(b.as_bytes(), [1, 2, 255]);
            drop(b.take_vec());
            let mut out = std::mem::MaybeUninit::<VeltStr>::uninit();
            velt_host_str_new(out.as_mut_ptr(), filled(b"a\xffb"), 3);
            let mut s = out.assume_init();
            assert_eq!(s.as_bytes(), "a\u{fffd}b".as_bytes());
            s.release();
        }
    }

    #[test]
    fn no_host_has_no_slots() {
        velt_rt_host_release(3);
        assert_eq!(velt_rt_host_clone(3), 0);
    }
}
