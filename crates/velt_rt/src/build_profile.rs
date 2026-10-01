//! Marks a debug build of the runtime library. `velt build --release` links whatever runtime it
//! finds next to `velt`, and a debug one (no LTO, debug assertions, the checking allocator)
//! makes release programs several times slower without any other sign; `velt_link` looks for
//! this symbol in the archive's symbol index to warn about it (and `velt doctor` to report it).

/// Present only in debug builds; its value is never read.
#[cfg(debug_assertions)]
#[no_mangle]
pub static VELT_RT_DEBUG_BUILD: u8 = 1;
