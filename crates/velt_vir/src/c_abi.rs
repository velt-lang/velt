//! What lowering needs to know of a target's C library beyond `declare c` declarations
//! (docs/internals/design/c-ffi.md): where its `errno` is, and the triple of the machine the
//! compiler runs on. Every C function std and packages call is declared in Velt
//! (`declare c("c")`, checked against each target's headers); there is no table of them here.

/// A C function's errno, as lowering captures it right after a call whose declaration asks
/// (`declare c("c", { errno: true })`) into the slot `lastOsError()` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrnoSource {
    /// `*f()`: the C library's accessor of the calling thread's `errno` (`__error` on macOS,
    /// `__errno_location` on Linux, `_errno` in Windows' C runtime).
    Accessor(&'static str),
    /// `GetLastError()`: a Windows library other than the C runtime reports its errors there.
    LastError,
}

/// Where a call of a C function of `library` leaves its error on `target`; `None` where no
/// errno is captured (WebAssembly).
pub fn errno_source(target: &str, library: &str) -> Option<ErrnoSource> {
    if !native(target) {
        return None;
    }
    if target.contains("-apple-") || target.contains("-darwin") {
        Some(ErrnoSource::Accessor("__error"))
    } else if target.contains("-windows") {
        Some(match library {
            "c" => ErrnoSource::Accessor("_errno"),
            _ => ErrnoSource::LastError,
        })
    } else {
        Some(ErrnoSource::Accessor("__errno_location"))
    }
}

/// The thread-local slot `lastOsError()` reads: std's `const OS_ERROR = new ThreadLocal<i32>(0)`
/// in std/os/rt.velt, named as lowering names a `ThreadLocal`'s storage (its qualified name and
/// VIR type, lower/thread_local.rs), so a capture here and `OS_ERROR.get()` there are one value.
pub const ERRNO_SLOT: &str = "std/os/rt::OS_ERROR:I32";

/// Is `target` a native one (not WebAssembly), whose C library every program links?
pub fn native(target: &str) -> bool {
    !target.trim().starts_with("wasm")
}

/// The triple of the machine the compiler runs on (lowering without a target: tests and the
/// JIT).
pub fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        os => format!("{arch}-unknown-{os}"),
    }
}

/// The C symbol a declaration named `declared` links: its name without a `$suffix`, so one
/// module can declare one C function with several signatures (`fcntl$lock` is `fcntl`).
pub fn base_name(declared: &str) -> &str {
    declared.split('$').next().unwrap_or(declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_per_target() {
        let accessor = |t: &str, lib: &str| match errno_source(t, lib) {
            Some(ErrnoSource::Accessor(s)) => s,
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(accessor("aarch64-apple-darwin", "c"), "__error");
        assert_eq!(
            accessor("x86_64-unknown-linux-musl", "c"),
            "__errno_location"
        );
        assert_eq!(accessor("x86_64-pc-windows-msvc", "c"), "_errno");
        assert_eq!(
            errno_source("aarch64-pc-windows-msvc", "kernel32"),
            Some(ErrnoSource::LastError)
        );
        assert_eq!(errno_source("wasm32-wasip1", "c"), None);
        assert_eq!(base_name("fcntl$lock"), "fcntl");
        assert_eq!(base_name("fcntl"), "fcntl");
    }
}
