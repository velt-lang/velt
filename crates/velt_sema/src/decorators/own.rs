//! `@own()` on an instance member of a std class (std/lang/intrinsics/decorators.velt): JavaScript
//! installs that member as an own property of every instance (an error's `stack`), so a subclass
//! member of the same name never replaces it. The subclass member is still declared and its body
//! checked, as TypeScript does, but no lookup reaches it: `collect::sigs` leaves it out of the
//! class's method table and `collect::classes` gives it no override.
//!
//! Found by the role `decorator_own`, not by the name a use is written with. Collection runs
//! before uses are recorded, so the ancestors' declarations are read here, resolving each decorator
//! as `known::intrinsics` does; [`check`] reports a use anywhere else.

use velt_common::{std_hook, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::FnKind;
use crate::hir::{DecoratorTarget, DefId};

/// The decorator's definition, when its module is loaded.
fn role(cx: &Ctx) -> Option<DefId> {
    match cx.std_def(std_hook!(decorator_own)) {
        Some(Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// The names of the instance members `@own()` marks in the std classes class `d` extends,
/// directly or not. Empty for a class without a base, the common case, at the cost of one look.
pub(crate) fn inherited(cx: &Ctx, d: DefId) -> Vec<String> {
    let mut out = vec![];
    let mut cur = base_of(cx, d);
    let Some(want) = cur.and_then(|_| role(cx)) else {
        return out;
    };
    let mut guard = 0;
    while let Some(c) = cur {
        guard += 1;
        if guard > 64 {
            break;
        }
        let Some(a) = cx.adt(c) else { break };
        if let (Some(decl), true) = (a.decl, cx.declared_in_std(c)) {
            for m in decl.methods.iter().filter(|m| !m.is_static) {
                let marked = (m.decl.sig.decorators.iter())
                    .any(|dec| super::known::written_def(cx, a.module, dec) == Some(want));
                let name = &m.decl.sig.name.name;
                if marked && !out.contains(name) {
                    out.push(name.clone());
                }
            }
        }
        cur = base_of(cx, c);
    }
    out
}

fn base_of(cx: &Ctx, d: DefId) -> Option<DefId> {
    let base = cx.adt(d)?.base?;
    cx.class_of(base).map(|(b, _)| b)
}

/// Is method `m` of a class that inherits the `@own()` members `own` ([`inherited`]) one they
/// hide?
pub(crate) fn hidden(own: &[String], m: &ast::Method) -> bool {
    !m.is_static && own.contains(&m.decl.sig.name.name)
}

/// A use of `@own()` on function `d` is on an instance member of a class declared in std.
pub(super) fn check(cx: &mut Ctx, d: DefId) {
    let Some(want) = role(cx) else { return };
    let Some(uses) = cx.decorator_uses.get(&DecoratorTarget::Def(d)) else {
        return;
    };
    let Some(span): Option<Span> = uses.iter().find(|u| u.def == want).map(|u| u.span) else {
        return;
    };
    let f = cx.fn_info(d);
    let in_std_class = f.kind == FnKind::Method && f.owner.is_some_and(|o| cx.declared_in_std(o));
    if !in_std_class {
        cx.err(
            "`@own` applies to an instance member of a class declared in std",
            span,
        );
    }
}
