//! The check in front of a loop and the copy it runs where the check passes.

use velt_vir::vir::{
    BinOp, BlockId, Const, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use super::{Decided, Limit, Plan, BOUND_LIMIT};
use crate::noalias::add_block;
use crate::visit::successors_mut;

/// Copy `plan`'s loop with its decided tests `true`, and send every edge into its head through
/// the check, which goes to the copy where it passes and to the original loop elsewhere.
pub(super) fn apply(func: &mut Function, plan: &Plan) {
    // Blocks a copy or check of an earlier loop added are past `plan.body`, outside this loop.
    let n = func.blocks.len();
    let inside = |b: usize| plan.body.get(b) == Some(&true);
    let mut map: Vec<Option<u32>> = vec![None; n];
    let mut next = n as u32;
    for (b, m) in map.iter_mut().enumerate() {
        if inside(b) {
            *m = Some(next);
            next += 1;
        }
    }
    for b in (0..n).filter(|&b| inside(b)) {
        copy_block(func, plan, &map, b);
    }
    let fast = BlockId(map[plan.head].expect("ICE: loop head is in its body"));
    let entry = check(func, plan, fast);
    let head = BlockId(plan.head as u32);
    for b in (0..n).filter(|&b| !inside(b)) {
        successors_mut(&mut func.blocks[b].term, &mut |t| {
            if *t == head {
                *t = entry;
            }
        });
    }
}

/// Append the copy of block `b`: its edges inside the loop go to the copies, and a decided test
/// is `dest = true` (at the call's location) then a jump.
fn copy_block(func: &mut Function, plan: &Plan, map: &[Option<u32>], b: usize) {
    let mut block = func.blocks[b].clone();
    successors_mut(&mut block.term, &mut |t| {
        if let Some(&Some(m)) = map.get(t.0 as usize) {
            *t = BlockId(m);
        }
    });
    let mut locs = func.locs.get(b).cloned();
    for d in &plan.decided {
        if let Decided::Compare(db, s) = *d {
            if db == b {
                let Stmt::Assign(_, rv) = &mut block.stmts[s] else {
                    panic!("ICE: a decided comparison is an assignment");
                };
                *rv = Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool));
            }
        }
    }
    let call = plan.decided.iter().find_map(|d| match *d {
        Decided::Call(db, dest) if db == b => Some(dest),
        _ => None,
    });
    if let Some(dest) = call {
        let Terminator::Call { next, .. } = block.term else {
            panic!("ICE: a decided test is a call");
        };
        let at = block.stmts.len();
        block.stmts.push(Stmt::Assign(
            Place::local(dest),
            Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
        ));
        block.term = Terminator::Goto(next);
        if let Some(l) = &mut locs {
            let call = l.last().copied().flatten();
            l.insert(at.min(l.len()), call);
        }
    }
    func.blocks.push(block);
    if !func.locs.is_empty() {
        func.locs.push(locs.unwrap_or_default());
    }
}

/// Build the check in front of the loop (module doc): a chain of tests, each going on to the
/// next where it holds and to the original head where it does not, the last to `fast`. Returns
/// its first block.
fn check(func: &mut Function, plan: &Plan, fast: BlockId) -> BlockId {
    let slow = BlockId(plan.head as u32);
    let u64c = |v: u64| Operand::Const(Const::Int(v.into()), Ty::U64);
    let copy = |l: Local| Operand::Copy(Place::local(l));
    // Each test: the statements computing it, and the bool that must hold.
    let mut tests: Vec<(Vec<Stmt>, Local)> = vec![];
    let mut cx = Chain {
        func,
        tests: &mut tests,
    };
    let gap = u64c(plan.gap);
    let ok = cx.local(Ty::Bool);
    cx.test(
        vec![bin(ok, BinOp::Ge, plan.bound.clone(), gap.clone())],
        ok,
    );
    let dg = cx.local(Ty::U64);
    let ok = cx.local(Ty::Bool);
    cx.test(
        vec![
            bin(dg, BinOp::Sub, plan.bound.clone(), gap),
            bin(ok, BinOp::Le, copy(plan.counter), copy(dg)),
        ],
        ok,
    );
    let ok = cx.local(Ty::Bool);
    cx.test(vec![bin(ok, BinOp::Le, copy(dg), u64c(BOUND_LIMIT))], ok);
    for r in &plan.ranges {
        if let Limit::Length(array) = r.limit {
            let ok = cx.local(Ty::Bool);
            let null = Operand::Const(Const::Int(0), Ty::Ptr);
            cx.test(vec![bin(ok, BinOp::Ne, copy(array), null)], ok);
        }
        let mut stmts = vec![];
        let first = match r.base {
            Some(base) => {
                let x = cx.local(Ty::U64);
                let ok = cx.local(Ty::Bool);
                cx.test(
                    vec![
                        bin(x, BinOp::Add, copy(base), copy(dg)),
                        bin(ok, BinOp::Ge, copy(x), copy(dg)),
                    ],
                    ok,
                );
                x
            }
            None => dg,
        };
        let (at, len, last, ok) = (
            cx.local(Ty::Ptr),
            cx.local(Ty::U64),
            cx.local(Ty::U64),
            cx.local(Ty::Bool),
        );
        stmts.push(bin(last, BinOp::Add, copy(first), u64c(r.reach)));
        stmts.push(bin(ok, BinOp::Ge, copy(last), copy(first)));
        cx.test(stmts, ok);
        let fits = cx.local(Ty::Bool);
        match &r.limit {
            Limit::Length(array) => {
                let header_len = Place {
                    local: at,
                    proj: vec![Proj::Deref(Ty::U64)],
                };
                cx.test(
                    vec![
                        bin(at, BinOp::PtrAdd, copy(*array), u64c(8)),
                        Stmt::Assign(Place::local(len), Rvalue::Use(Operand::Copy(header_len))),
                        bin(fits, BinOp::Le, copy(last), copy(len)),
                    ],
                    fits,
                );
            }
            Limit::Bound(n) => {
                cx.test(vec![bin(fits, BinOp::Le, copy(last), n.clone())], fits);
            }
        }
    }
    let mut next = fast;
    for (stmts, ok) in tests.into_iter().rev() {
        next = add_block(
            func,
            stmts,
            Terminator::Branch {
                cond: copy(ok),
                then: next,
                els: slow,
            },
        );
    }
    next
}

/// The tests of a check being built, with the locals they need.
struct Chain<'a> {
    func: &'a mut Function,
    tests: &'a mut Vec<(Vec<Stmt>, Local)>,
}

impl Chain<'_> {
    fn local(&mut self, ty: Ty) -> Local {
        self.func.locals.push(LocalDecl::new(ty, None));
        Local(self.func.locals.len() as u32 - 1)
    }

    fn test(&mut self, stmts: Vec<Stmt>, ok: Local) {
        self.tests.push((stmts, ok));
    }
}

fn bin(dest: Local, op: BinOp, a: Operand, b: Operand) -> Stmt {
    Stmt::Assign(Place::local(dest), Rvalue::Binary(op, a, b))
}
