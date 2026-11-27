//! The few services the runtime needs from its host, per target:
//! - WASI (`wasm32-wasip1`) and native hosts (unit tests): Rust `std` (WASI `fd_write`,
//!   `proc_exit`);
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

/// The program's command-line arguments (argv[0] included; empty in the browser).
pub fn args() -> Vec<String> {
    std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}
