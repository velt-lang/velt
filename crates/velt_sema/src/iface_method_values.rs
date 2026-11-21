//! An interface's method read as the value of an object-literal property (`{ setKey:
//! handle.setKey }`, sigx's async engine). Node then calls the function with the literal as
//! `this`, and Velt keeps the object the method was read from, so the two differ only for an
//! implementation that reads `this`. Which implementations an interface has is known once every
//! body is checked: the read is accepted while checking (`body/expr/literal_this.rs`
//! `stored_method_read`) and reported here when some method of the same name may read `this`:
//!
//! - a method of an object literal that uses `this` (`MethodValues::this_slots`);
//! - a class's method whose body uses `this` or `super`;
//! - an interface's default body, when it uses them.
//!
//! Any interface's or class's method of that name counts: an adapter fills an interface's slot
//! with the method of the value it adapts (a literal of its own interface, a value of an
//! interface of the same shape, a class instance), and a subclass's override is called through
//! its base class's slot. A function Velt makes itself (a literal's function field) has no
//! `this` of its own.
//!
//! The fact is the whole program's: std's stage of a separate check (`crate::units`) and a kept
//! check's reload (`crate::kept_reload`) keep the reads for the check that sees every body.

use rustc_hash::FxHashSet;
use velt_common::Span;

use crate::collect::static_this::uses_this;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::DefId;

/// What this check gathers while bodies are checked (`ObjectImpls::method_values`).
#[derive(Clone, Debug, Default)]
pub(crate) struct MethodValues {
    /// The interface slots an object literal fills with a method reading `this`.
    pub this_slots: FxHashSet<(DefId, u32)>,
    /// The interface methods read as object-literal properties' values.
    pub reads: Vec<StoredRead>,
}

/// An interface method read as an object-literal property's value.
#[derive(Clone, Debug)]
pub(crate) struct StoredRead {
    pub iface: DefId,
    pub name: String,
    /// The method as the message names it, and the read when it is a path.
    pub what: String,
    pub read: Option<String>,
    pub span: Span,
}

/// Report the reads of methods some implementation may call with its own `this`.
pub(crate) fn check(cx: &mut Ctx) {
    // std's stage alone sees part of the program: its unit keeps the reads for the app's stage.
    if cx.units_stage == crate::units::Stage::Std {
        return;
    }
    let reads = std::mem::take(&mut cx.object_impls.method_values.reads);
    let mut seen = FxHashSet::default();
    for r in reads {
        if !seen.insert((r.span.file, r.span.lo, r.span.hi)) {
            continue;
        }
        if may_read_this(cx, &r.name) {
            let d = crate::body::expr::stored_method_error(&r.what, r.read.as_deref(), r.span);
            cx.error(d);
        }
    }
}

/// May some implementation of a method named `name` read `this` (module docs)? Any interface's
/// counts, not only `iface`'s: an adapter fills one interface's slot with another interface's
/// method (a literal's own interface, an interface of the same shape, a class's), and a class's
/// subclasses implement its interfaces' methods too.
fn may_read_this(cx: &Ctx, name: &str) -> bool {
    let named = |d: DefId, slot: usize| {
        cx.iface(d)
            .and_then(|i| i.methods.get(slot))
            .is_some_and(|m| m.name == name)
    };
    let literal =
        (cx.object_impls.method_values.this_slots.iter()).any(|&(d, slot)| named(d, slot as usize));
    if literal {
        return true;
    }
    cx.info.iter().any(|info| match info {
        DefInfo::Iface(i) => i
            .methods
            .iter()
            .any(|m| m.name == name && m.default.is_some_and(|d| uses_this(cx, d))),
        DefInfo::Adt(a) => a
            .methods
            .get(name)
            .is_some_and(|m| !m.is_static && uses_this(cx, m.def)),
        _ => false,
    })
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    MethodValues { this_slots, reads }
    StoredRead { iface, name, what, read, span }
}
