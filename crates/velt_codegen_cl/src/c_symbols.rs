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
    fn pthread_key_create(key: *mut u64, dtor: *const c_void) -> i32;
    fn pthread_key_delete(key: u64) -> i32;
    fn pthread_getspecific(key: u64) -> *mut c_void;
    fn pthread_setspecific(key: u64, v: *const c_void) -> i32;
    fn fmaf(x: f32, y: f32, z: f32) -> f32;
}

/// The address of C library function `name`, if generated code may call it.
pub(crate) fn lookup(name: &str) -> Option<*const u8> {
    let address = match name {
        "memcpy" => memcpy as *const u8,
        // A `ThreadLocal`'s thread keys (tls.rs: the JIT has no thread-local storage).
        "pthread_key_create" => pthread_key_create as *const u8,
        "pthread_key_delete" => pthread_key_delete as *const u8,
        "pthread_getspecific" => pthread_getspecific as *const u8,
        "pthread_setspecific" => pthread_setspecific as *const u8,
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
        _ => return system(name),
    };
    Some(address)
}

/// The C library functions std declares with `declare c("c")` (velt/fs's file descriptors,
/// std/fs/fd/posix.velt) and the `errno` accessor lowering calls after them (velt_vir's `c_abi`),
/// by the symbols they link, and the rest of the file functions `declare system function` had
/// (`fdatasync`, `lseek`), so a program's own `declare c("c")` of them still runs under a fully
/// static `velt run`. Their signatures do not matter here, only their addresses.
#[cfg(unix)]
fn system(name: &str) -> Option<*const u8> {
    mod c {
        use std::ffi::{c_char, c_int, c_void};
        // Only addresses are taken; Rust checks the signatures of the ones its standard library
        // uses too, so they are the C library's (`off_t` is 64 bits on every target here).
        // `fstat` is the plain symbol std links, the 32-bit-inode one on x86-64 macOS, whose
        // `struct stat` std/fs/fd/darwin.velt reads there.
        extern "C" {
            pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
            pub fn close(fd: c_int) -> c_int;
            pub fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
            pub fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
            pub fn pread(fd: c_int, buf: *mut c_void, n: usize, at: i64) -> isize;
            pub fn pwrite(fd: c_int, buf: *const c_void, n: usize, at: i64) -> isize;
            pub fn fsync(fd: c_int) -> c_int;
            #[cfg(not(target_os = "macos"))]
            pub fn fdatasync(fd: c_int) -> c_int;
            pub fn ftruncate(fd: c_int, len: i64) -> c_int;
            pub fn lseek(fd: c_int, at: i64, whence: c_int) -> i64;
            pub fn unlink(path: *const c_char) -> c_int;
            pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
            pub fn fstat(fd: c_int, st: *mut c_void) -> c_int;
            pub fn malloc(n: usize) -> *mut c_void;
            pub fn free(p: *mut c_void);
            #[cfg(target_os = "macos")]
            pub fn __error() -> *mut c_int;
            #[cfg(not(target_os = "macos"))]
            pub fn __errno_location() -> *mut c_int;
        }
    }
    let address = match name {
        "open" => c::open as *const u8,
        "close" => c::close as *const u8,
        "read" => c::read as *const u8,
        "write" => c::write as *const u8,
        "pread" => c::pread as *const u8,
        "pwrite" => c::pwrite as *const u8,
        "fsync" => c::fsync as *const u8,
        #[cfg(not(target_os = "macos"))]
        "fdatasync" => c::fdatasync as *const u8,
        "ftruncate" => c::ftruncate as *const u8,
        "lseek" => c::lseek as *const u8,
        "unlink" => c::unlink as *const u8,
        "fcntl" => c::fcntl as *const u8,
        "fstat" => c::fstat as *const u8,
        "malloc" => c::malloc as *const u8,
        "free" => c::free as *const u8,
        #[cfg(target_os = "macos")]
        "__error" => c::__error as *const u8,
        #[cfg(not(target_os = "macos"))]
        "__errno_location" => c::__errno_location as *const u8,
        _ => return None,
    };
    Some(address)
}

/// Windows keeps `cranelift-jit`'s lookup through the loaded modules (kernel32's included).
#[cfg(not(unix))]
fn system(_: &str) -> Option<*const u8> {
    None
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

    /// Every C function std's fd API declares on this host, the `errno` accessor, and `lseek`
    /// (a program's own declaration of it worked as a system function) have an address
    /// (`velt run` calls them in the JIT, also in a static `velt`).
    #[cfg(unix)]
    #[test]
    fn covers_std_s_c_functions() {
        let errno = if cfg!(target_os = "macos") {
            "__error"
        } else {
            "__errno_location"
        };
        let mut names = vec![
            "open",
            "close",
            "read",
            "write",
            "pread",
            "pwrite",
            "fsync",
            "ftruncate",
            "lseek",
            "unlink",
            "fcntl",
            "fstat",
            "malloc",
            "free",
            errno,
        ];
        if !cfg!(target_os = "macos") {
            names.push("fdatasync");
        }
        for name in names {
            assert!(super::lookup(name).is_some(), "no address for `{name}`");
        }
    }
}
