//! Ownership passes over the checked HIR (after every body is checked, before the move
//! dataflow): [`infer`] decides which params / receivers / pattern bindings take ownership or
//! are modified ([`evidence`], [`mutation`]) and patches call sites accordingly ([`patch`],
//! [`finish`]); [`fn_values`] keeps borrowed closures from outliving the call they were
//! passed to; [`validate`] rejects moves out of borrowed places;
//! [`soft`] turns async-call arguments that must not be moved into clones, and [`strings`]
//! makes every string move soft (strings are values); [`exclusive`]
//! (last) rejects calls where a place the callee may modify is reachable through another
//! argument.

mod evidence;
mod exclusive;
mod finish;
mod fn_values;
mod infer;
mod mutation;
mod patch;
mod soft;
mod strings;
mod validate;
mod worklist;

pub(crate) use exclusive::check_exclusive;
pub(crate) use infer::infer_modes;
pub(crate) use soft::clone_reused;
pub(crate) use strings::soften_string_moves;
pub(crate) use validate::validate_moves;
