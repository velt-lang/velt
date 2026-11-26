//! The runtime's entries of the debug owner check of captured variables' cells (#916).
//!
//! A variable that a closure assigns while something else still sees it lives in a counted heap
//! cell (`LocalDef::boxed`, velt_vir `lower/cells.rs`), which must never be used by two tasks.
//! The executor in Velt checks that (std/lang/hooks/sched_cells.velt): lowering calls its hooks
//! wherever it runs the program. A program that runs nothing on the executor calls these instead
//! (velt_vir `lower/sched.rs`). The runtime no longer polls the program's code as tasks of its
//! own, so it has no task to tell apart, and they do nothing.

use crate::str::VeltStr;

/// A new cell at `cell`, holding variable `name`.
#[no_mangle]
pub extern "C" fn velt_rt_cell_new(cell: *const u8, name: *const VeltStr) {
    let _ = (cell, name);
}

/// The program uses `cell` (changes its count, or calls a closure assigning it).
#[no_mangle]
pub extern "C" fn velt_rt_cell_use(cell: *const u8) {
    let _ = cell;
}

/// The only reference to `cell` moves to another task (transfer glue).
#[no_mangle]
pub extern "C" fn velt_rt_cell_give(cell: *const u8) {
    let _ = cell;
}

/// `cell` is a fresh copy of `from` made for another task (transfer glue).
#[no_mangle]
pub extern "C" fn velt_rt_cell_copy(cell: *const u8, from: *const u8) {
    let _ = (cell, from);
}

/// `cell` is about to be freed.
#[no_mangle]
pub extern "C" fn velt_rt_cell_free(cell: *const u8) {
    let _ = cell;
}
