//! The C library functions generated code may call (`fmod` for `%` on floats, `memcpy` & co. for
//! runtime-length copies, and Cranelift's libcalls), by address, for the JIT.
//!
//! `cranelift-jit` would otherwise look them up with `dlsym`, which finds nothing in a static
//! executable: a fully static musl `velt` has no dynamic loader. The host already has the C
//! library linked in, so the addresses come from these declarations instead, on every Unix (one
//! path for glibc, musl and macOS). Windows keeps `cranelift-jit`'s lookup through the loaded
//! CRT modules.

use std::ffi::c_void;

extern "C" {
    fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn memset(dst: *mut c_void, byte: i32, n: usize) -> *mut c_void;
    fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32;
    fn fmod(x: f64, y: f64) -> f64;
    fn fmodf(x: f32, y: f32) -> f32;
    fn ceil(x: f64) -> f64;
    fn ceilf(x: f32) -> f32;
    fn floor(x: f64) -> f64;
    fn floorf(x: f32) -> f32;
    fn trunc(x: f64) -> f64;
    fn truncf(x: f32) -> f32;
    fn nearbyint(x: f64) -> f64;
    fn nearbyintf(x: f32) -> f32;
    fn fma(x: f64, y: f64, z: f64) -> f64;
    fn fmaf(x: f32, y: f32, z: f32) -> f32;
}

/// The address of C library function `name`, if generated code may call it.
pub(crate) fn lookup(name: &str) -> Option<*const u8> {
    let address = match name {
        "memcpy" => memcpy as *const u8,
        "memmove" => memmove as *const u8,
        "memset" => memset as *const u8,
        "memcmp" => memcmp as *const u8,
        "fmod" => fmod as *const u8,
        "fmodf" => fmodf as *const u8,
        "ceil" => ceil as *const u8,
        "ceilf" => ceilf as *const u8,
        "floor" => floor as *const u8,
        "floorf" => floorf as *const u8,
        "trunc" => trunc as *const u8,
        "truncf" => truncf as *const u8,
        "nearbyint" => nearbyint as *const u8,
        "nearbyintf" => nearbyintf as *const u8,
        "fma" => fma as *const u8,
        "fmaf" => fmaf as *const u8,
        _ => return None,
    };
    Some(address)
}

#[cfg(test)]
mod tests {
    use cranelift_codegen::ir::LibCall;

    /// Every libcall Cranelift may emit for our targets (plus `fmod`/`fmodf`) has an address.
    #[test]
    fn covers_the_libcalls() {
        let libcalls = [
            LibCall::CeilF32,
            LibCall::CeilF64,
            LibCall::FloorF32,
            LibCall::FloorF64,
            LibCall::TruncF32,
            LibCall::TruncF64,
            LibCall::NearestF32,
            LibCall::NearestF64,
            LibCall::FmaF32,
            LibCall::FmaF64,
            LibCall::Memcpy,
            LibCall::Memset,
            LibCall::Memmove,
            LibCall::Memcmp,
        ];
        let names = cranelift_module::default_libcall_names();
        for name in libcalls
            .into_iter()
            .map(names)
            .chain(["fmod".into(), "fmodf".into()])
        {
            assert!(super::lookup(&name).is_some(), "no address for `{name}`");
        }
    }
}
