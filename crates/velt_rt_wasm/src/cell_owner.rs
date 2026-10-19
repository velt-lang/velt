//! The debug runtime's owner check of captured variables' cells (velt_rt `cell_owner.rs`): a
//! WebAssembly program runs on one thread, so no cell can be used by two at once, and these are
//! empty.

use crate::str::VeltStr;

#[no_mangle]
pub extern "C" fn velt_rt_cell_new(_cell: *const u8, _name: *const VeltStr) {}

#[no_mangle]
pub extern "C" fn velt_rt_cell_use(_cell: *const u8) {}

#[no_mangle]
pub extern "C" fn velt_rt_cell_give(_cell: *const u8) {}

#[no_mangle]
pub extern "C" fn velt_rt_cell_copy(_cell: *const u8, _from: *const u8) {}

#[no_mangle]
pub extern "C" fn velt_rt_cell_free(_cell: *const u8) {}
