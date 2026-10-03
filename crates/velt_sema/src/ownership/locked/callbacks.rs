//! Which closures `with` calls (super module docs): the closures the argument may be
//! (`super::values`: a literal, a local bound only to closure literals, a conditional, a
//! captured variable, what a function returns: `m.with(makeCb(out))`), and, through a
//! function parameter that reaches `with`, the closures passed for it (to a fixpoint). Any other function value is *opaque*: its body is
//! not visible, and only its type is checked.

use std::collections::{HashMap, HashSet};

use super::values::Resolver;
use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, Stmt, StmtKind as S,
};
use crate::visit::{self, VisitMut};

#[derive(Default)]
pub(super) struct Found {
    /// Closures `with` calls, and whether the literal is `with`'s argument itself.
    pub(super) callbacks: Vec<(DefId, bool)>,
    /// Opaque callbacks: the argument expressions.
    pub(super) opaque: Vec<Expr>,
    /// `(function, parameter index)` of parameters passed to `with` as the callback.
    params: HashSet<(DefId, usize)>,
}

/// Every callback of the program's `with` calls.
pub(super) fn find(cx: &mut Ctx, res: &mut Resolver) -> Found {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    let mut found = Found::default();
    for &d in &fns {
        let calls = calls_in(cx, d, |c, _| {
            matches!(c, Callee::Intrinsic(Intrinsic::MutexWith))
        });
        for (args, params) in calls {
            if let [_, cb] = args.as_slice() {
                resolve(cx, res, d, cb, &params, true, &mut found);
            }
        }
    }
    loop {
        let before = found.params.len();
        for &d in &fns {
            let locked = found.params.clone();
            let calls = calls_in(
                cx,
                d,
                |c, i| matches!(c, Callee::Def(g, _) if locked.contains(&(*g, i))),
            );
            for (args, params) in calls {
                for a in args {
                    resolve(cx, res, d, &a, &params, false, &mut found);
                }
            }
        }
        if found.params.len() == before {
            return found;
        }
    }
}

/// A callback argument of a call in function `d`.
fn resolve(
    cx: &mut Ctx,
    res: &mut Resolver,
    d: DefId,
    cb: &Expr,
    params: &[LocalId],
    at_with: bool,
    found: &mut Found,
) {
    let direct = at_with && matches!(cb.kind, E::Closure(_));
    if let E::Local(l, _) = cb.kind {
        let closure = cx.fn_info(d).kind == FnKind::Closure;
        if let (Some(i), false) = (params.iter().position(|p| *p == l), closure) {
            found.params.insert((d, i));
            return;
        }
    }
    match res.expr(cx, d, cb) {
        Some(cs) => found.callbacks.extend(cs.into_iter().map(|c| (c, direct))),
        None => found.opaque.push(cb.clone()),
    }
}

/// The arguments of the calls in function `d` that `pick(callee, argument index)` selects
/// (all of a call's arguments when any is selected), with `d`'s parameters.
type Calls = Vec<(Vec<Expr>, Vec<LocalId>)>;

fn calls_in(cx: &mut Ctx, d: DefId, pick: impl Fn(&Callee, usize) -> bool) -> Calls {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return vec![];
    };
    let mut args_found: Vec<Vec<Expr>> = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Call { callee, args } = &e.kind {
            let picked: Vec<Expr> = (0..args.len())
                .filter(|i| pick(callee, *i))
                .map(|i| args[i].clone())
                .collect();
            if matches!(callee, Callee::Intrinsic(Intrinsic::MutexWith)) && !picked.is_empty() {
                args_found.push(args.clone());
            } else if !picked.is_empty() {
                args_found.push(picked);
            }
        }
    });
    let params: Vec<LocalId> = f.params.iter().map(|p| p.local).collect();
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    args_found
        .into_iter()
        .map(|a| (a, params.clone()))
        .collect()
}

/// Locals every value of which is a closure literal (its `let`/`const` initializer and every
/// assignment), with those closures.
pub(super) fn closure_locals(b: &mut Block) -> HashMap<LocalId, Vec<DefId>> {
    #[derive(Default)]
    struct Lets {
        closures: HashMap<LocalId, Vec<DefId>>,
        other: HashSet<LocalId>,
    }
    impl Lets {
        fn value(&mut self, l: LocalId, e: &Expr) {
            match e.kind {
                E::Closure(c) => self.closures.entry(l).or_default().push(c),
                _ => {
                    self.other.insert(l);
                }
            }
        }
    }
    impl VisitMut for Lets {
        fn stmt(&mut self, s: &mut Stmt) {
            match &s.kind {
                S::Let {
                    local,
                    init: Some(init),
                } => self.value(*local, init),
                S::Let { local, init: None } => {
                    self.closures.entry(*local).or_default();
                }
                _ => {}
            }
        }
        fn expr(&mut self, e: &mut Expr) {
            if let E::Assign { place, value } = &e.kind {
                if let E::Local(l, _) = place.kind {
                    let value = (**value).clone();
                    self.value(l, &value);
                }
            }
        }
    }
    let mut v = Lets::default();
    visit::block(b, &mut v);
    let Lets { closures, other } = v;
    closures
        .into_iter()
        .filter(|(l, cs)| !other.contains(l) && !cs.is_empty())
        .collect()
}
