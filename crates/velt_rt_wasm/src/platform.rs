//! The few services the runtime needs from its host, per target:
//! - WASI (`wasm32-wasip1`) and native hosts (unit tests): Rust `std` (WASI `fd_write`,
//!   `proc_exit`, `clock_time_get`, `poll_oneoff`);
//! - the browser (`wasm32-unknown-unknown`): imports from the `velt` module that the JS glue
//!   provides (`editors/web/velt_web.js`), because `std`'s I/O, clocks and exit are stubs there.

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// Write `len` bytes at `ptr` to stream 1 (stdout) or 2 (stderr).
        pub fn write(stream: u32, ptr: *const u8, len: usize);
        /// Stop the program with `code` (the glue throws, so this never returns).
        pub fn exit(code: i32) -> !;
        /// `performance.now()`.
        pub fn now_ms() -> f64;
        /// `Date.now()`.
        pub fn date_ms() -> f64;
    }
}

/// Write `bytes` to stream 1 (stdout) or 2 (stderr), ignoring errors like `console.log` does.
pub fn write(stream: u32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: the glue reads `len` bytes of linear memory at `ptr`, which `bytes` covers.
    unsafe {
        imports::write(stream, bytes.as_ptr(), bytes.len())
    };
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        use std::io::Write;
        let _ = if stream == 2 {
            std::io::stderr().lock().write_all(bytes)
        } else {
            let mut out = std::io::stdout().lock();
            out.write_all(bytes).and_then(|()| out.flush())
        };
    }
}

/// End the process with `code` (the caller flushed stdout).
pub fn exit(code: i32) -> ! {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: the import never returns (the glue throws).
    unsafe {
        imports::exit(code)
    }
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    std::process::exit(code)
}

/// Milliseconds on a monotonic clock (arbitrary origin).
pub fn monotonic_ms() -> f64 {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: a plain import without arguments.
    return unsafe { imports::now_ms() };
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
    }
}

/// Milliseconds since the Unix epoch.
pub fn epoch_ms() -> i64 {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    // SAFETY: a plain import without arguments.
    return unsafe { imports::date_ms() } as i64;
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Block the (only) thread for `ms` milliseconds: the executor's wait for its next timer. The
/// browser cannot block its thread, so there this spins on the clock (programs run in a worker).
pub fn sleep_ms(ms: f64) {
    if ms <= 0.0 {
        return;
    }
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    {
        let end = monotonic_ms() + ms;
        while monotonic_ms() < end {
            std::hint::spin_loop();
        }
    }
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
}

/// The program's command-line arguments (argv[0] included; empty in the browser).
pub fn args() -> Vec<String> {
    std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_advance() {
        let a = monotonic_ms();
        sleep_ms(2.0);
        assert!(monotonic_ms() - a >= 2.0);
        assert!(epoch_ms() > 1_600_000_000_000);
    }
}
