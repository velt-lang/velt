//! HTTP handler descriptors across versions. A running server holds a `VeltHandler { init,
//! poll, drop, state_size, state_align, env }` of the version that started it; `init`, `poll`
//! and `drop` are pinned code of one version (they share its state layout). When a swap
//! recompiles a handler, the host gives the server a new descriptor with the new version's code
//! and state layout (the environment is unchanged: its layout did not change, or the swap would
//! be a restart).

use velt_vir::vir::{Function, Local, Program, Proj, Ty};

/// The code and state layout a handler's descriptor needs (addresses as integers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandlerCode {
    /// `init(env, req, state)`.
    pub init: usize,
    /// The handler state machine's poll function.
    pub poll: usize,
    /// The handler state machine's drop function.
    pub drop: usize,
    /// Size of the handler state in bytes.
    pub state_size: u64,
    /// Alignment of the handler state.
    pub state_align: u64,
}

/// The poll and drop keys of the handler whose `init` key is `init_key` (`<closure>$init`, or
/// `<closure>$copy$init`, the one whose requests copy some captures: velt_vir
/// `async_fn/handler.rs`).
pub(crate) fn state_machine_keys(init_key: &str) -> Option<(String, String)> {
    let base = init_key
        .strip_suffix("$copy$init")
        .or_else(|| init_key.strip_suffix("$init"))?;
    Some((format!("{base}$poll"), format!("{base}$drop")))
}

/// Size and alignment of the state an `init(env, req, state)` function writes: the aggregate
/// its `state` parameter (local 2) is dereferenced as.
pub(crate) fn state_layout(init: &Function, program: &Program) -> Option<(u64, u64)> {
    const STATE_PARAM: Local = Local(2);
    let places = init
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter_map(|s| match s {
            velt_vir::vir::Stmt::Assign(place, _) => Some(place),
            _ => None,
        });
    for place in places {
        if place.local != STATE_PARAM {
            continue;
        }
        if let Some(Proj::Deref(Ty::Agg(agg))) = place.proj.first() {
            let layout = program.aggs.get(agg.0 as usize)?;
            return Some((u64::from(layout.size), u64::from(layout.align)));
        }
    }
    None
}
