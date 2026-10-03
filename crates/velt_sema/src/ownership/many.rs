//! Values reachable from several threads at once: the argument of `shared(x)` (every thread
//! holding the `shared` reaches it) and the handler passed to a function that hands it to an
//! HTTP server (`Intrinsic::HttpHandler`: requests run concurrently). A function value there
//! may be called from several threads at once, and each call shares what the function captured
//! with the call (velt_vir async_fn/ctor.rs `take_capture`), so a captured resource without
//! `clone()` would have its count updated from several threads. Where the function is visible
//! here — a closure literal in the argument, or a `const` bound to one — that is an error;
//! otherwise the many-threads check where the value is made panics (velt_vir glue/many.rs).

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalDef, LocalId, PassMode, Stmt,
    StmtKind as S, TyId,
};
use crate::visit::{self, VisitMut};

/// Where a value becomes reachable from several threads.
#[derive(Clone, Copy)]
enum Point {
    Shared,
    Handler,
}

/// Report closures reachable from several threads at once that capture a resource without
/// `clone()` (module docs).
pub(crate) fn check_many_threads(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let handlers = handler_params(cx, &fns);
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let bound = closure_consts(&mut f.body.block, &f.body.locals);
        let mut points: Vec<(Point, Vec<DefId>)> = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let E::Call { callee, args } = &e.kind else {
                return;
            };
            match callee {
                Callee::Intrinsic(Intrinsic::SharedNew) => {
                    for a in args {
                        points.push((Point::Shared, closures_in(a, &bound)));
                    }
                }
                Callee::Def(g, _) => {
                    for (i, a) in args.iter().enumerate() {
                        if handlers.contains(&(*g, i)) {
                            points.push((Point::Handler, closures_in(a, &bound)));
                        }
                    }
                }
                _ => {}
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        let mut reported = HashSet::new();
        for (point, closures) in points {
            for c in closures {
                if reported.insert(c) {
                    check_closure(cx, c, point);
                }
            }
        }
    }
}

/// `(function, parameter index)` of parameters that a function captures in the closure it
/// passes to `Intrinsic::HttpHandler` (std/http.vlt `serve`'s `handler`).
fn handler_params(cx: &Ctx, fns: &[DefId]) -> HashSet<(DefId, usize)> {
    let mut out = HashSet::new();
    for &d in fns {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        let mut block = f.body.block.clone();
        let mut handlers = vec![];
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Intrinsic(Intrinsic::HttpHandler),
                args,
            } = &e.kind
            {
                if let Some(E::Closure(c)) = args.first().map(|a| &a.kind) {
                    handlers.push(*c);
                }
            }
        });
        for c in handlers {
            let Some(Def::Fn(h)) = &cx.defs[c.0 as usize] else {
                continue;
            };
            for k in &h.captures {
                if let Some(i) = f.params.iter().position(|p| p.local == k.outer) {
                    out.insert((d, i));
                }
            }
        }
    }
    out
}

/// `const` locals initialized with a closure literal.
fn closure_consts(b: &mut crate::hir::Block, locals: &[LocalDef]) -> HashMap<LocalId, DefId> {
    struct Lets<'a>(&'a [LocalDef], HashMap<LocalId, DefId>);
    impl VisitMut for Lets<'_> {
        fn stmt(&mut self, s: &mut Stmt) {
            if let S::Let {
                local,
                init: Some(init),
            } = &s.kind
            {
                if let (E::Closure(c), false) = (&init.kind, self.0[local.0 as usize].mutable) {
                    self.1.insert(*local, *c);
                }
            }
        }
    }
    let mut v = Lets(locals, HashMap::new());
    visit::block(b, &mut v);
    v.1
}

/// The closures `e` makes reachable that are visible here: literals in it and `const`s bound
/// to one.
fn closures_in(e: &Expr, bound: &HashMap<LocalId, DefId>) -> Vec<DefId> {
    let mut e = e.clone();
    let mut out = vec![];
    visit::expr(&mut e, &mut Found(bound, &mut out));
    out
}

struct Found<'a, 'b>(&'a HashMap<LocalId, DefId>, &'b mut Vec<DefId>);

impl VisitMut for Found<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(c) => self.1.push(*c),
            E::Local(l, _) => {
                if let Some(c) = self.0.get(l) {
                    self.1.push(*c);
                }
            }
            _ => {}
        }
    }
}

/// Report the captures of closure `c` that each call would share although they own a
/// resource without `clone()`.
fn check_closure(cx: &mut Ctx, c: DefId, point: Point) {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return;
    };
    let span = f.span;
    let caps: Vec<(String, TyId)> = f
        .captures
        .iter()
        .filter(|k| k.mode == PassMode::Owned)
        .map(|k| {
            let l = &f.body.locals[k.inner.0 as usize];
            (l.name.clone(), l.ty)
        })
        .collect();
    for (name, ty) in caps {
        if cx.owns_uncopyable(ty) {
            report(cx, span, &name, ty, point);
        }
    }
}

fn report(cx: &mut Ctx, span: Span, name: &str, ty: TyId, point: Point) {
    let what = cx.uncopyable_why(ty);
    let part = cx.uncopyable_part(ty).unwrap_or(ty);
    let pn = cx.display(part);
    let place = match point {
        Point::Shared => "it is in `shared(...)`, so several threads may call it at once",
        Point::Handler => "it handles HTTP requests, which run on several threads at once",
    };
    cx.error(
        Diagnostic::error(
            format!("this function captures `{name}`, and {place}: each call would share `{name}` across threads, but {what}"),
            span,
        )
        .with_note(format!(
            "give `{pn}` a `clone()` method that duplicates the resource (each call then gets its own copy), or capture a shared one: `shared(new Mutex(…))`"
        )),
    );
}
