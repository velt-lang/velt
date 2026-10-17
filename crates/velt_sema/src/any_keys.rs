//! `Object.keys` of an `any` (issues #924, #926). Its keys come from the class of the value
//! the `any` holds (`body::expr::object_keys`): right when every value the program converts to
//! a dynamic type is an instance of one of its classes, and wrong for anything else (an array,
//! a string, an object type, a function), whose keys the descriptor has no slot for yet. Once
//! every body is checked, [`check`] looks at what the program converts (`ToDyn`, outside the
//! standard library) and reports each `Object.keys` of an `any` as not supported yet when any
//! of those values is not a class instance. `null` and `undefined` don't count.

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Def, Expr, ExprKind as E, TyId};
use crate::visit::{self, VisitMut};

/// Report the `Object.keys` calls of an `any` (`Ctx::any_keys`) if the program converts a
/// value that is not a class instance to a dynamic type.
pub(crate) fn check(cx: &mut Ctx) {
    let mut sites = std::mem::take(&mut cx.any_keys);
    if sites.is_empty() {
        return;
    }
    let Some(any_impl) = cx.any.and_then(|a| a.imp) else {
        return;
    };
    let found = first_non_class(cx, any_impl);
    let Some(t) = found else {
        return;
    };
    // A body checked twice (`body::recheck`) records its calls twice.
    let mut seen = std::collections::HashSet::new();
    sites.retain(|s| seen.insert(*s));
    let tn = cx.display(t);
    for span in sites {
        cx.error(
            Diagnostic::error(
                "`Object.keys` of an `any` value is not supported yet (#924, #926)",
                span,
            )
            .with_note(format!(
                "it lists the fields of a class instance; this program also converts `{tn}` to a dynamic type"
            ))
            .with_note("convert the value to its class or object type first (`x as T`, checked at run time)"),
        );
    }
}

/// The first type the program's own code converts to a dynamic type whose values are not all
/// class instances (or `null` / `undefined`).
fn first_non_class(cx: &mut Ctx, any_impl: u32) -> Option<TyId> {
    let mut defs = std::mem::take(&mut cx.defs);
    let mut found = Found {
        cx: &mut *cx,
        any_impl,
        first: None,
    };
    for (i, d) in defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let std = found
            .cx
            .try_fn(crate::hir::DefId(i as u32))
            .is_none_or(|fi| found.cx.scopes[fi.module].is_std);
        if !std {
            visit::block(&mut f.body.block, &mut found);
        }
    }
    let mut inits: Vec<Expr> = vec![];
    for info in &found.cx.info {
        let (module, xs): (usize, Vec<Expr>) = match info {
            DefInfo::Global(g) => (g.module, g.init.iter().cloned().collect()),
            DefInfo::Adt(a) => (
                a.module,
                a.fields.iter().filter_map(|f| f.default.clone()).collect(),
            ),
            _ => continue,
        };
        if !found.cx.modules.get(module).is_some_and(|m| m.is_std) {
            inits.extend(xs);
        }
    }
    for mut x in inits {
        visit::expr(&mut x, &mut found);
    }
    let first = found.first;
    cx.defs = defs;
    first
}

struct Found<'a, 'c> {
    cx: &'a mut Ctx<'c>,
    any_impl: u32,
    first: Option<TyId>,
}

impl VisitMut for Found<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        if self.first.is_some() {
            return;
        }
        if let E::ToDyn { expr, impl_index } = &e.kind {
            if *impl_index == self.any_impl && !holds_classes(self.cx, expr.ty) {
                self.first = Some(expr.ty);
            }
        }
    }
}

/// Is every value of `t` a class instance of the program, a dynamic value or a nothing?
fn holds_classes(cx: &mut Ctx, t: TyId) -> bool {
    if cx.dynamic(t).is_some() {
        return true;
    }
    if let Some(p) = cx.ty.opt_payload(t) {
        // `boolean | null` is what a dynamic `null` / `undefined` holds.
        return p == cx.ty.bool_ || holds_classes(cx, p);
    }
    if let Some(ms) = cx.union_members(t) {
        return ms.into_iter().all(|m| holds_classes(cx, m));
    }
    cx.class_of(t)
        .and_then(|(d, _)| cx.adt(d))
        .is_some_and(|a| !cx.scopes[a.module].is_std)
}
