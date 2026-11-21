//! An interface's method read as the value of an object-literal property (`{ setKey:
//! handle.setKey }`, sigx's async engine). Node then calls the function with the literal as
//! `this`, and Velt keeps the object the method was read from, so the two differ only for an
//! implementation that reads `this`. Which implementations an interface has is known once every
//! body is checked: the read is accepted while checking (`body/expr/literal_this.rs`
//! `stored_method_read`) and reported here when some implementation of the method, of the
//! interface or of one extending it, may read `this`:
//!
//! - a method of an object literal that uses `this` (`MethodValues::this_slots`);
//! - a class's method whose body uses `this` or `super`;
//! - the interface's default body, when it uses them.
//!
//! An implementation Velt makes itself (a literal's function field, an adapter) runs the stored
//! function, which reads no `this` of its own.

use rustc_hash::FxHashSet;
use velt_common::Span;
use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

use crate::ctx::Ctx;
use crate::defs::FnSource;
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
    let reads = std::mem::take(&mut cx.object_impls.method_values.reads);
    let mut seen = FxHashSet::default();
    for r in reads {
        if !seen.insert((r.span.file, r.span.lo, r.span.hi)) {
            continue;
        }
        if may_read_this(cx, r.iface, &r.name) {
            let d = crate::body::expr::stored_method_error(&r.what, r.read.as_deref(), r.span);
            cx.error(d);
        }
    }
}

/// Does `ancestor` appear among `d` and the interfaces it extends?
fn extends(cx: &Ctx, d: DefId, ancestor: DefId, depth: u32) -> bool {
    if d == ancestor {
        return true;
    }
    if depth > 32 {
        return false;
    }
    cx.iface(d).is_some_and(|i| {
        i.parents
            .iter()
            .any(|p| extends(cx, p.iface, ancestor, depth + 1))
    })
}

/// May some implementation of method `name` of interface `iface` read `this` (module docs)?
fn may_read_this(cx: &Ctx, iface: DefId, name: &str) -> bool {
    let slot_of = |d: DefId| {
        cx.iface(d)
            .and_then(|i| i.methods.iter().position(|m| m.name == name))
    };
    let literal = (cx.object_impls.method_values.this_slots.iter())
        .any(|&(d, slot)| extends(cx, d, iface, 0) && slot_of(d) == Some(slot as usize));
    if literal {
        return true;
    }
    let default = slot_of(iface)
        .and_then(|k| cx.iface(iface)?.methods[k].default)
        .is_some_and(|m| fn_reads_this(cx, m));
    if default {
        return true;
    }
    cx.impls.iter().any(|imp| {
        extends(cx, imp.iface, iface, 0)
            && slot_of(imp.iface)
                .and_then(|k| imp.methods.get(k).copied())
                .is_some_and(|m| fn_reads_this(cx, m))
    })
}

/// Does function `m`'s written body use `this` or `super`? A function Velt made has none.
fn fn_reads_this(cx: &Ctx, m: DefId) -> bool {
    struct Finder(bool);
    impl<'a> Visit<'a> for Finder {
        fn expr(&mut self, e: &'a ast::Expr) {
            self.0 |= matches!(e.kind, ast::ExprKind::This | ast::ExprKind::Super);
        }
    }
    let mut f = Finder(false);
    match cx.fn_info(m).source {
        Some(FnSource::Decl(decl)) => visit::walk_fn(decl, &mut f),
        Some(FnSource::Default(_, body)) => visit::walk_block(body, &mut f),
        None => {}
    }
    f.0
}

// The encoding of a finished unit (`units::codec`): every field, so a new one fails to build.
crate::units::codec::structs! {
    MethodValues { this_slots, reads }
    StoredRead { iface, name, what, read, span }
}
