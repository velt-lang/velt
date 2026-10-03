//! Which closures `with` calls (super module docs): the closure literal passed to it, a local
//! bound only to closure literals (`const f = (v) => …`, `let f = …`), the closures a function
//! returns (`m.with(makeCb(out))`), and, through a function parameter that reaches `with`, the
//! closures passed for it (to a fixpoint). Any other function value is *opaque*: its body is
//! not visible, and only its type is checked.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::BodyState;
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
pub(super) fn find(cx: &mut Ctx) -> Found {
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
        for (args, bound, params) in calls {
            if let [_, cb] = args.as_slice() {
                resolve(cx, d, cb, &bound, &params, true, &mut found);
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
            for (args, bound, params) in calls {
                for a in args {
                    resolve(cx, d, &a, &bound, &params, false, &mut found);
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
    d: DefId,
    cb: &Expr,
    bound: &HashMap<LocalId, Vec<DefId>>,
    params: &[LocalId],
    at_with: bool,
    found: &mut Found,
) {
    let direct = at_with && matches!(cb.kind, E::Closure(_));
    let closures = match &cb.kind {
        E::Closure(c) => Some(vec![*c]),
        E::Local(l, _) => match params.iter().position(|p| p == l) {
            Some(i) => {
                found.params.insert((d, i));
                return;
            }
            None => bound.get(l).cloned(),
        },
        E::Call {
            callee: Callee::Def(g, _),
            ..
        } => returned_closures(cx, *g),
        _ => None,
    };
    match closures {
        Some(cs) => found.callbacks.extend(cs.into_iter().map(|c| (c, direct))),
        None => found.opaque.push(cb.clone()),
    }
}

/// The arguments of the calls in function `d` that `pick(callee, argument index)` selects
/// (all of a call's arguments when any is selected), with the locals of `d` bound only to
/// closures and `d`'s parameters.
type Calls = Vec<(Vec<Expr>, HashMap<LocalId, Vec<DefId>>, Vec<LocalId>)>;

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
    let bound = if args_found.is_empty() {
        HashMap::new()
    } else {
        closure_locals(&mut f.body.block)
    };
    let params: Vec<LocalId> = f.params.iter().map(|p| p.local).collect();
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    args_found
        .into_iter()
        .map(|a| (a, bound.clone(), params.clone()))
        .collect()
}

/// Locals every value of which is a closure literal (its `let`/`const` initializer and every
/// assignment), with those closures.
fn closure_locals(b: &mut Block) -> HashMap<LocalId, Vec<DefId>> {
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

/// The closures function `g` returns, when every value it returns is a closure literal.
fn returned_closures(cx: &mut Ctx, g: DefId) -> Option<Vec<DefId>> {
    struct Returns {
        closures: Vec<DefId>,
        other: bool,
    }
    impl VisitMut for Returns {
        fn stmt(&mut self, s: &mut Stmt) {
            if let S::Return(e) = &s.kind {
                match e.as_ref().map(|e| &e.kind) {
                    Some(E::Closure(c)) => self.closures.push(*c),
                    _ => self.other = true,
                }
            }
        }
    }
    let Some(Def::Fn(mut f)) = cx.defs[g.0 as usize].take() else {
        return None;
    };
    let mut r = Returns {
        closures: vec![],
        other: f.body.block.value.is_some(),
    };
    visit::block(&mut f.body.block, &mut r);
    cx.defs[g.0 as usize] = Some(Def::Fn(f));
    (!r.other && !r.closures.is_empty()).then_some(r.closures)
}
