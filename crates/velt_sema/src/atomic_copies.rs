//! An `Atomic<T>` is a word in place (docs/internals/design/atomics.md): it cannot be shared,
//! which would make it a second reference to one word and give its type a count (boxing it in
//! every program). This pass, after the ownership passes (which add shares), reports each
//! share of one in a body, each closure capture that shares one, each binding of one in a
//! destructuring declaration (`const { hits } = c`), and each share of a part
//! of a `shared` value holding one (`const xs = s.items`): lowering makes that a deep copy, so
//! the atomics changed through it would be the copy's. Lowering reports the copies this pass
//! cannot see (by-reference bindings, arguments through function values: velt_vir
//! `lower/atomic.rs`) and shares made in generic code (`lower/boxing`).

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, Expr, ExprKind as E, Intrinsic, Pat, PatKind, Stmt, StmtKind, TyId};
use crate::visit;

pub(crate) fn check(cx: &mut Ctx) {
    if cx.atomic_ty().is_none() {
        return;
    }
    let mut found: Vec<(TyId, Span)> = vec![];
    let mut frozen: Vec<(TyId, Span)> = vec![];
    // Shares that are the value of a `shared(...)`: `shared(a)` of an `Atomic` still used.
    let mut into_shared: Vec<Span> = vec![];
    for d in cx.defs.iter_mut() {
        let Some(Def::Fn(f)) = d else { continue };
        for c in f.captures.iter().filter(|c| c.share) {
            let l = &f.body.locals[c.inner.0 as usize];
            found.push((l.ty, l.span));
        }
        // `const { hits } = c`: a binding of a destructuring declaration is another
        // reference to the field (a `for … of` binding of an element is the element in place).
        let mut lets = LetBindings(vec![]);
        visit::block(&mut f.body.block, &mut lets);
        found.extend(lets.0);
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::SharedNew),
                args,
            } = &e.kind
            {
                into_shared.extend(args.first().map(|a| a.span));
            }
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } = &e.kind
            {
                found.push((e.ty, e.span));
                if args.first().is_some_and(in_shared) {
                    frozen.push((e.ty, e.span));
                }
            }
        });
    }
    found.sort_by_key(|(_, s)| (s.file, s.lo, s.hi));
    let mut seen = rustc_hash::FxHashSet::default();
    for (t, span) in frozen {
        if cx.atomic_value(t).is_some() || !cx.holds_atomic(t) || !seen.insert(span) {
            continue;
        }
        cx.error(
            Diagnostic::error(
                "this copies a part of a `shared` value that holds an `Atomic`: changes to the copy's atomics would not reach the shared value",
                span,
            )
            .with_note("use it in place (`s.items[i].count.add(1)`, `for (const x of s.items)`), or pass it to a function that only reads or calls its methods"),
        );
    }
    for (t, span) in found {
        if cx.atomic_value(t).is_none() || !seen.insert(span) {
            continue;
        }
        let shown = cx.display(t);
        let note = match into_shared.contains(&span) {
            true => format!("create it inside the `shared`: `shared(new {shown}(…))`"),
            false => "read it with `.load()`, or pass or keep the object that holds it".to_string(),
        };
        cx.error(
            Diagnostic::error(
                format!("an `{shown}` cannot be copied or shared: it is used where it is"),
                span,
            )
            .with_note(note),
        );
    }
}

/// The bindings of the destructuring declarations (`LetPat`) visited, with their spans.
struct LetBindings(Vec<(TyId, Span)>);

impl visit::VisitMut for LetBindings {
    fn stmt(&mut self, s: &mut Stmt) {
        if let StmtKind::LetPat { pat, .. } = &s.kind {
            bindings(pat, &mut self.0);
        }
    }
}

fn bindings(p: &Pat, out: &mut Vec<(TyId, Span)>) {
    match &p.kind {
        PatKind::Binding(..) => out.push((p.ty, p.span)),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|x| bindings(x, out))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|x| bindings(x, out)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| bindings(x, out)),
        PatKind::Some(x) => bindings(x, out),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
    }
}

/// Is `e` a part of the value in a `shared` box, read in place (`Intrinsic::SharedRead`)?
fn in_shared(e: &Expr) -> bool {
    match &e.kind {
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            ..
        } => true,
        E::Field { base, .. } | E::Index { base, .. } | E::UnwrapSome(base, _) => in_shared(base),
        _ => false,
    }
}
