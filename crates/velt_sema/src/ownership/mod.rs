//! Ownership passes over the checked HIR (after every body is checked, before the move
//! dataflow): [`local_closures`] first makes closures held in a `const` and only called
//! non-escaping, and [`local_async`] decides which async closures stay on their task;
//! [`infer`] decides which params / receivers / pattern bindings take ownership or are
//! modified ([`evidence`], [`mutation`]) and patches call sites accordingly ([`patch`],
//! [`finish`]); [`fn_values`] keeps borrowed closures from outliving the call they were
//! passed to; [`validate`] rejects moves out of borrowed places;
//! [`soft`] turns async-call arguments that must not be moved into shares, and [`shares`]
//! makes every move of a shared value soft (semantics stage 2); [`exclusive`]
//! rejects calls where a place the callee may modify is reachable through another
//! argument; [`boundary`] rejects resources that `spawn` would have to copy, [`locked`]
//! checks what `Mutex.with` callbacks let past the lock, and [`many`] (last) closures callable
//! from several threads at once that would share one.

mod boundary;
mod cells;
mod evidence;
mod exclusive;
mod finish;
mod fn_values;
mod infer;
mod local_async;
mod local_closures;
mod locked;
mod many;
mod mutation;
mod patch;
mod shares;
mod soft;
mod validate;
mod worklist;

pub(crate) use boundary::check_boundaries;
pub(crate) use cells::{
    box_cells, unshareable_nested, unshareable_note, Unshareable, NESTED_ASSIGN,
};
pub(crate) use exclusive::check_exclusive;
pub(crate) use infer::infer_modes;
pub(crate) use local_async::infer_local_async;
pub(crate) use local_closures::{demote_local_closures, held_closures};
pub(crate) use locked::check_locked;
pub(crate) use many::check_many_threads;
pub(crate) use shares::soften_moves;
pub(crate) use soft::clone_reused;
pub(crate) use validate::validate_moves;
