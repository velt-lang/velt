//! Recognising values read from freed blocks (#872): the checking allocator of the debug
//! runtime (`VELT_RT_DEBUG_ALLOC=1`, `debug_alloc`) fills freed blocks with one word, which it
//! publishes here ([`set_poison`]); runtime entry points that take a value generated code may
//! have read from memory check it with [`check_value`]. Shared with every runtime build (the
//! wasm one and the host's too), where nothing publishes a word and the check never fires; the
//! release runtime compiles the check out.

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering};

/// The word freed blocks hold (0: no checking allocator, nothing to recognise).
#[cfg(debug_assertions)]
static POISON: AtomicU64 = AtomicU64::new(0);

/// Publish the poison word of the checking allocator (once, before its first free).
#[cfg(debug_assertions)]
#[allow(dead_code)] // only the checking allocator publishes one
pub(crate) fn set_poison(word: u64) {
    POISON.store(word, Ordering::Relaxed);
}

/// Abort with `use after free: <what>` when `word`, a value the runtime was handed or read from
/// a block it was handed, is the poison of a freed block. Compiled out of the release runtime;
/// one comparison when the checking allocator is off.
#[inline(always)]
pub fn check_value(word: u64, what: &str) {
    #[cfg(debug_assertions)]
    {
        let p = POISON.load(Ordering::Relaxed);
        if p != 0 && word == p {
            use_after_free(what, word as usize);
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (word, what);
}

#[cfg(debug_assertions)]
#[cold]
#[inline(never)]
fn use_after_free(what: &str, addr: usize) -> ! {
    use std::io::Write;
    let mut buf = [0u8; 256];
    let n = message(&mut buf, what, addr);
    let _ = std::io::stderr().write_all(&buf[..n]);
    std::process::abort()
}

/// Write `velt debug-alloc: use after free: <what> (address …)` and a newline into `buf`
/// without allocating (the fault handler uses it too); returns its length.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub(crate) fn message(buf: &mut [u8; 256], what: &str, addr: usize) -> usize {
    use std::io::Write;
    let mut w = &mut buf[..];
    let _ = writeln!(
        w,
        "velt debug-alloc: use after free: {what} (address {addr:#x})"
    );
    256 - w.len()
}
