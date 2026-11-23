//! A function stored as an object-literal property that it isn't written in (`{ setKey:
//! handle.setKey }`, sigx's async engine, a variable, a call's result). Node calls it with the
//! literal as `this` (`o.setKey()`), while Velt keeps the object a method read as a value was
//! read from (#746), so the two differ for a method that reads `this`. Where the value came from
//! isn't followed (a variable, `?:`, `||`, a function's result, an array): the rule is on the
//! property's name. A function value not written in the literal (`body/expr/literal_this.rs`
//! `stored_value`) is refused when some method of the same name may read `this`, once every
//! body is checked:
//!
//! - a method of an object literal that uses `this` (`MethodValues::this_slots`);
//! - a class's method (static or not) whose body uses `this` or `super`;
//! - an interface's default body, when it uses them.
//!
//! Std's own interfaces and classes don't count: their method names (`get`, `add`, `push`) are
//! everywhere, and a std method stored this way throws in Node (an incompatible receiver)
//! rather than giving another result.
//!
//! Any interface's or class's method of that name counts: an adapter fills an interface's slot
//! with the method of the value it adapts, and a subclass's override is called through its base
//! class's slot. A function written in the literal (an arrow, a function expression, a method)
//! is the literal's own. A name no method reading `this` has is accepted whatever the value.
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
    /// The functions stored as object-literal properties they aren't written in.
    pub reads: Vec<StoredRead>,
}

/// A function not written in an object literal, stored as its property `name`
/// (`body/expr/literal_this.rs` `stored_value`).
#[derive(Clone, Debug)]
pub(crate) struct StoredRead {
    pub name: String,
    /// The value when it is a path (`g.greet`, `f`), for the fix the message suggests.
    pub read: Option<String>,
    pub span: Span,
}

/// Report the functions stored under the name of a method that may read `this`.
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
            let d = crate::body::expr::stored_method_error(&r.name, r.read.as_deref(), r.span);
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
    // Std's own types don't count (module docs).
    let std = |module: usize| cx.scopes[module].is_std;
    cx.info.iter().any(|info| match info {
        DefInfo::Iface(i) if !std(i.module) => i
            .methods
            .iter()
            .any(|m| m.name == name && m.default.is_some_and(|d| uses_this(cx, d))),
        DefInfo::Adt(a) if !std(a.module) => {
            a.methods.get(name).is_some_and(|m| uses_this(cx, m.def))
        }
        _ => false,
    })
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    MethodValues { this_slots, reads }
    StoredRead { name, read, span }
}
