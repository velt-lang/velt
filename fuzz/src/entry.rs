//! Process entry for the fuzz binaries. `velt_rt`'s rlib defines the C `main` (it calls the
//! compiled program's `velt_main`), which wins over libFuzzer's `main` at link time. So this
//! crate provides `velt_main` and starts libFuzzer from there with `LLVMFuzzerRunDriver`.

use std::ffi::{c_char, c_int, CString};

extern "C" {
    /// Defined by `libfuzzer_sys::fuzz_target!` in each target.
    fn LLVMFuzzerTestOneInput(data: *const u8, size: usize) -> c_int;
    /// libFuzzer's driver entry (the body of its own `main`).
    fn LLVMFuzzerRunDriver(
        argc: *mut c_int,
        argv: *mut *mut *mut c_char,
        callback: unsafe extern "C" fn(*const u8, usize) -> c_int,
    ) -> c_int;
}

/// Called by `velt_rt`'s `main` after its runtime init.
#[no_mangle]
pub extern "C" fn velt_main() -> i32 {
    // The runtime's panic hook reports and exits like a Velt program; libFuzzer needs the default
    // hook (then libfuzzer-sys aborts) to recognize a crash and save the input.
    drop(std::panic::take_hook());
    let args: Vec<CString> = std::env::args_os()
        .map(|a| CString::new(a.to_string_lossy().into_owned()).expect("ICE: argument with NUL"))
        .collect();
    // libFuzzer may keep pointers into argv for the whole run: leak it.
    let mut ptrs: Vec<*mut c_char> = args.into_iter().map(CString::into_raw).collect();
    ptrs.push(std::ptr::null_mut());
    let mut argc = (ptrs.len() - 1) as c_int;
    let mut argv = Box::leak(ptrs.into_boxed_slice()).as_mut_ptr();
    // SAFETY: argc/argv describe NUL-terminated strings that live forever; the callback is the
    // target's libFuzzer entry point.
    unsafe { LLVMFuzzerRunDriver(&mut argc, &mut argv, LLVMFuzzerTestOneInput) }
}
