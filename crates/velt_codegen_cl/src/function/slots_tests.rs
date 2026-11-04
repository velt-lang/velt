//! Slot sharing (`slots::plan`, #2048): locals whose live ranges are disjoint share a slot;
//! overlapping, escaped and described locals do not.

use velt_vir::vir::{
    self, BinOp, BlockId, Callee, Const, DebugTyId, ExternFn, ExternId, Local, LocalDebug,
    LocalDecl, Operand, Place, Program, Proj, Rvalue, SrcLoc, Stmt, Terminator, Ty, STR_AGG,
};

use super::slots::{plan, plan_counted, WALK_BUDGET};
use crate::tests::{copy_local, int, str_agg, FuncBuilder};

const S: Ty = Ty::Agg(STR_AGG);

fn program() -> Program {
    Program {
        aggs: vec![str_agg()],
        externs: vec![
            // 0: use(ptr)
            ExternFn {
                symbol: "use_str".into(),
                params: vec![Ty::Ptr],
                ret: Ty::Unit,
                noreturn: false,
                host: None,
                c_decl: None,
            },
            // 1: keep(ptr) -> ptr (returns its argument)
            ExternFn {
                symbol: "keep".into(),
                params: vec![Ty::Ptr],
                ret: Ty::Ptr,
                noreturn: false,
                host: None,
                c_decl: None,
            },
        ],
        funcs: vec![callee("ret.out"), callee("s")],
        ..Default::default()
    }
}

/// Function 0, `mk(ret.out: ptr)`, writes its result through its out-pointer; function 1,
/// `take(s: ptr)`, borrows a string.
const MK: Callee = Callee::Func(vir::FuncId(0));
const TAKE: Callee = Callee::Func(vir::FuncId(1));

fn callee(param: &str) -> vir::Function {
    let mut fb = FuncBuilder::internal(param, &[Ty::Ptr], Ty::Unit);
    let b = fb.block();
    ret(b, &mut fb);
    let mut f = fb.finish();
    f.locals[0].name = Some(param.into());
    f
}

fn str_value() -> Rvalue {
    Rvalue::Aggregate(
        STR_AGG,
        vec![int(1, Ty::U64), int(2, Ty::U64), int(3, Ty::U64)],
    )
}

fn field(l: Local, n: u32) -> Operand {
    Operand::Copy(Place {
        local: l,
        proj: vec![Proj::Field(n)],
    })
}

fn memory(f: &vir::Function) -> Vec<bool> {
    f.locals
        .iter()
        .map(|l| matches!(l.ty, Ty::Agg(_)))
        .collect()
}

fn slots_of(f: &vir::Function, in_memory: &[bool]) -> (Vec<Option<u32>>, usize) {
    let p = plan(&program(), f, in_memory, &|i| f.locals[i].debug.is_some()).unwrap();
    (p.local_slot, p.slots.len())
}

fn ret(b: BlockId, fb: &mut FuncBuilder) {
    fb.term(b, Terminator::Return(Operand::Const(Const::Unit, Ty::Unit)));
}

/// `n` string temporaries, each written and read before the next is written, in one block.
fn sequential(n: usize) -> vir::Function {
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let b = fb.block();
    fb.assign(b, sum, Rvalue::Use(int(0, Ty::U64)));
    for _ in 0..n {
        let t = fb.local(S);
        fb.assign(b, t, str_value());
        fb.assign(
            b,
            sum,
            Rvalue::Binary(BinOp::Add, copy_local(sum), field(t, 1)),
        );
    }
    ret(b, &mut fb);
    fb.finish()
}

#[test]
fn sequential_temporaries_share_one_slot() {
    let f = sequential(1000);
    let (slots, count) = slots_of(&f, &memory(&f));
    assert_eq!(count, 1, "1000 disjoint temporaries need one slot");
    assert!(slots[1..].iter().all(|s| *s == Some(0)));
}

#[test]
fn overlapping_temporaries_get_their_own_slots() {
    // a = …; b = …; read a; read b: both live between the two writes.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let a = fb.local(S);
    let c = fb.local(S);
    let b = fb.block();
    fb.assign(b, a, str_value());
    fb.assign(b, c, str_value());
    fb.assign(b, sum, Rvalue::Use(field(a, 0)));
    fb.assign(b, sum, Rvalue::Use(field(c, 0)));
    ret(b, &mut fb);
    let f = fb.finish();
    let (slots, count) = slots_of(&f, &memory(&f));
    assert_eq!(count, 2);
    assert_ne!(slots[a.0 as usize], slots[c.0 as usize]);
}

#[test]
fn a_value_carried_around_a_loop_stays_live_in_the_whole_loop() {
    // bb0: a = …; goto bb1
    // bb1: t = …; read t; read a; branch bb1 / bb2   (a is live across the back edge)
    // bb2: return
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let a = fb.local(S);
    let t = fb.local(S);
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, a, str_value());
    fb.assign(
        b0,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    fb.term(b0, Terminator::Goto(b1));
    fb.assign(b1, t, str_value());
    fb.assign(b1, sum, Rvalue::Use(field(t, 0)));
    fb.assign(b1, sum, Rvalue::Use(field(a, 0)));
    fb.term(
        b1,
        Terminator::Branch {
            cond: copy_local(cond),
            then: b1,
            els: b2,
        },
    );
    ret(b2, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[a.0 as usize], slots[t.0 as usize]);
}

#[test]
fn a_value_read_in_a_loop_written_before_it_overlaps_the_loop_temporaries() {
    // bb0: t0 = …; goto bb1
    // bb1: read t0; u = …; read u; branch bb1 / bb2  — t0 is read in every iteration, so u
    // (written after the read in block order) must not share with it either.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let t0 = fb.local(S);
    let u = fb.local(S);
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, t0, str_value());
    fb.assign(
        b0,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    fb.term(b0, Terminator::Goto(b1));
    fb.assign(b1, sum, Rvalue::Use(field(t0, 0)));
    fb.assign(b1, u, str_value());
    fb.assign(b1, sum, Rvalue::Use(field(u, 0)));
    fb.term(
        b1,
        Terminator::Branch {
            cond: copy_local(cond),
            then: b1,
            els: b2,
        },
    );
    ret(b2, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[t0.0 as usize], slots[u.0 as usize]);
}

#[test]
fn a_pointer_use_keeps_the_local_live() {
    // p = &a; a = …; b = …; read b; use_str(p): a is read through p after b's write.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let p = fb.local(Ty::Ptr);
    let a = fb.local(S);
    let c = fb.local(S);
    let b = fb.block();
    fb.assign(b, p, Rvalue::AddrOf(Place::local(a)));
    fb.assign(b, a, str_value());
    fb.assign(b, c, str_value());
    fb.assign(b, sum, Rvalue::Use(field(c, 0)));
    let next = fb.call(b, Callee::Extern(ExternId(0)), vec![copy_local(p)], None);
    ret(next, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[a.0 as usize], slots[c.0 as usize]);
}

#[test]
fn an_out_pointer_write_starts_the_range() {
    // a = …; read a; p = &b; use_str(p) (writes b); read b — then c = …; read c.
    // a's range ends before b's starts, and c starts after b's last read: one slot.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let p = fb.local(Ty::Ptr);
    let a = fb.local(S);
    let bl = fb.local(S);
    let c = fb.local(S);
    let b0 = fb.block();
    fb.assign(b0, a, str_value());
    fb.assign(b0, sum, Rvalue::Use(field(a, 0)));
    fb.assign(b0, p, Rvalue::AddrOf(Place::local(bl)));
    let b1 = fb.call(b0, Callee::Extern(ExternId(0)), vec![copy_local(p)], None);
    fb.assign(b1, sum, Rvalue::Use(field(bl, 0)));
    fb.assign(b1, c, str_value());
    fb.assign(b1, sum, Rvalue::Use(field(c, 0)));
    ret(b1, &mut fb);
    let f = fb.finish();
    let (slots, count) = slots_of(&f, &memory(&f));
    assert_eq!(count, 1);
    assert!(
        slots[a.0 as usize] == slots[bl.0 as usize] && slots[bl.0 as usize] == slots[c.0 as usize]
    );
}

#[test]
fn a_pointer_returned_by_a_call_is_followed() {
    // q = keep(&a); b = …; read b; read *q: a is still read after b's write.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let p = fb.local(Ty::Ptr);
    let q = fb.local(Ty::Ptr);
    let a = fb.local(S);
    let c = fb.local(S);
    let b0 = fb.block();
    fb.assign(b0, a, str_value());
    fb.assign(b0, p, Rvalue::AddrOf(Place::local(a)));
    let b1 = fb.call(
        b0,
        Callee::Extern(ExternId(1)),
        vec![copy_local(p)],
        Some(Place::local(q)),
    );
    fb.assign(b1, c, str_value());
    fb.assign(b1, sum, Rvalue::Use(field(c, 0)));
    fb.assign(
        b1,
        sum,
        Rvalue::Use(Operand::Copy(Place {
            local: q,
            proj: vec![Proj::Deref(S), Proj::Field(0)],
        })),
    );
    ret(b1, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[a.0 as usize], slots[c.0 as usize]);
}

#[test]
fn an_address_stored_in_memory_escapes() {
    // holder.0 = &a: `a` may be reached later through memory, so it keeps a slot of its own.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let p = fb.local(Ty::Ptr);
    let a = fb.local(S);
    let holder = fb.local(S);
    let c = fb.local(S);
    let b = fb.block();
    fb.assign(b, a, str_value());
    fb.assign(b, sum, Rvalue::Use(field(a, 0)));
    fb.assign(b, p, Rvalue::AddrOf(Place::local(a)));
    fb.push(
        b,
        Stmt::Assign(
            Place {
                local: holder,
                proj: vec![Proj::Field(0)],
            },
            Rvalue::Cast(copy_local(p), Ty::U64),
        ),
    );
    fb.assign(b, c, str_value());
    fb.assign(b, sum, Rvalue::Use(field(c, 0)));
    fb.assign(b, sum, Rvalue::Use(field(holder, 0)));
    ret(b, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    let sa = slots[a.0 as usize];
    for other in [holder, c] {
        assert_ne!(sa, slots[other.0 as usize]);
    }
}

#[test]
fn described_locals_and_params_keep_keyed_slots() {
    let mut f = sequential(3);
    f.locals[1].debug = Some(LocalDebug {
        decl: SrcLoc {
            file: 0,
            line: 1,
            col: 1,
        },
        end: None,
        ty: DebugTyId(0),
        by_ref: false,
        param: false,
    });
    f.locals[1].name = Some("s".into());
    let p = plan(&program(), &f, &memory(&f), &|i| {
        f.locals[i].debug.is_some()
    })
    .unwrap();
    let own = p.local_slot[1].unwrap() as usize;
    assert_eq!(p.slots[own].key, Some(1));
    assert!(p.local_slot[2..].iter().all(|s| *s != Some(own as u32)));
    assert_eq!(p.slots.len(), 2);
    // A parameter in memory keeps its own slot too.
    let mut g = sequential(2);
    g.params = vec![Ty::U64];
    g.locals[0] = LocalDecl::new(Ty::U64, None);
    let mut in_memory = memory(&g);
    in_memory[0] = true;
    let p = plan(&program(), &g, &in_memory, &|_| false).unwrap();
    assert_eq!(p.slots[p.local_slot[0].unwrap() as usize].key, Some(0));
}

/// `n` string results of calls of `callee` (`p = &t; callee(p)`, then a read of `t`), each in
/// blocks of its own; inside a loop when `in_loop`.
fn call_results(n: usize, callee: Callee, in_loop: bool) -> vir::Function {
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let entry = fb.block();
    fb.assign(entry, sum, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(
        entry,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    let head = fb.block();
    fb.term(entry, Terminator::Goto(head));
    let mut b = head;
    for _ in 0..n {
        let t = fb.local(S);
        let p = fb.local(Ty::Ptr);
        fb.assign(b, p, Rvalue::AddrOf(Place::local(t)));
        b = fb.call(b, callee.clone(), vec![copy_local(p)], None);
        fb.assign(
            b,
            sum,
            Rvalue::Binary(BinOp::Add, copy_local(sum), field(t, 0)),
        );
    }
    let exit = fb.block();
    let back = if in_loop { head } else { exit };
    fb.term(
        b,
        Terminator::Branch {
            cond: copy_local(cond),
            then: back,
            els: exit,
        },
    );
    ret(exit, &mut fb);
    fb.finish()
}

/// The slot count and the liveness walks' block visits of `f`.
fn plan_cost(f: &vir::Function) -> (usize, u64) {
    let (p, visits) = plan_counted(&program(), f, &memory(f), &|_| false).unwrap();
    (p.slots.len(), visits)
}

#[test]
fn liveness_work_grows_linearly_with_call_results() {
    // Each temporary's walk stops where no write can have happened (before its call), and in
    // a loop at the call that writes it through its out-pointer, so twice the temporaries is
    // twice the work. Walking back to the entry from each call made it four times.
    for (callee, in_loop) in [
        (Callee::Extern(ExternId(0)), false),
        (MK, false),
        (MK, true),
    ] {
        let (slots, small) = plan_cost(&call_results(500, callee.clone(), in_loop));
        let (_, large) = plan_cost(&call_results(1000, callee.clone(), in_loop));
        assert_eq!(slots, 1, "{callee:?} in a loop: {in_loop}");
        assert!(
            large <= 2 * small + 64 && large <= 8 * 1000,
            "{callee:?} in a loop: {in_loop}: {small} block visits for 500, {large} for 1000"
        );
    }
}

#[test]
fn an_out_pointer_call_in_a_loop_writes_the_whole_local() {
    // loop { p = &t; mk(p); read t; u = …; read u }: mk writes all of t, so t is dead across
    // the back edge and u can share its slot.
    let mut f2 = call_results(1, MK, true);
    let u = Local(f2.locals.len() as u32);
    f2.locals.push(LocalDecl::new(S, None));
    let last = BlockId(2);
    f2.blocks[last.0 as usize].stmts.extend([
        Stmt::Assign(Place::local(u), str_value()),
        Stmt::Assign(Place::local(Local(0)), Rvalue::Use(field(u, 0))),
    ]);
    let (slots2, count) = slots_of(&f2, &memory(&f2));
    assert_eq!(slots2[2], slots2[u.0 as usize]);
    assert_eq!(count, 1);
}

#[test]
fn a_borrowed_pointer_argument_in_a_loop_reads_the_local() {
    // t = … before the loop; loop { p = &t; take(p); read t; u = …; read u }: take only reads
    // t (its param is not an out-pointer), so t stays live around the loop and u needs a slot
    // of its own.
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let t = fb.local(S);
    let u = fb.local(S);
    let p = fb.local(Ty::Ptr);
    let (b0, b1) = (fb.block(), fb.block());
    fb.assign(b0, t, str_value());
    fb.assign(
        b0,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    fb.term(b0, Terminator::Goto(b1));
    fb.assign(b1, p, Rvalue::AddrOf(Place::local(t)));
    let b2 = fb.call(b1, TAKE, vec![copy_local(p)], None);
    fb.assign(b2, sum, Rvalue::Use(field(t, 0)));
    fb.assign(b2, u, str_value());
    fb.assign(b2, sum, Rvalue::Use(field(u, 0)));
    let b3 = fb.block();
    fb.term(
        b2,
        Terminator::Branch {
            cond: copy_local(cond),
            then: b1,
            els: b3,
        },
    );
    ret(b3, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[t.0 as usize], slots[u.0 as usize]);
}

#[test]
fn an_out_pointer_that_may_point_elsewhere_writes_nothing_whole() {
    // bb1: p = &t, or bb2: p = &a; u = …; read u — then mk(p): p is not exactly `&t`, so the
    // call only reads through it, and a (written before the branch, last used by the call)
    // overlaps u. (bb2 runs before bb1 in reverse postorder, so ts range doesnt cover u.)
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let a = fb.local(S);
    let t = fb.local(S);
    let u = fb.local(S);
    let p = fb.local(Ty::Ptr);
    let (b0, b1, b2, b3) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, a, str_value());
    fb.assign(
        b0,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    fb.term(
        b0,
        Terminator::Branch {
            cond: copy_local(cond),
            then: b1,
            els: b2,
        },
    );
    fb.assign(b1, p, Rvalue::AddrOf(Place::local(t)));
    fb.term(b1, Terminator::Goto(b3));
    fb.assign(b2, p, Rvalue::AddrOf(Place::local(a)));
    fb.assign(b2, u, str_value());
    fb.assign(b2, sum, Rvalue::Use(field(u, 0)));
    fb.term(b2, Terminator::Goto(b3));
    let b4 = fb.call(b3, MK, vec![copy_local(p)], None);
    ret(b4, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    assert_ne!(slots[a.0 as usize], slots[u.0 as usize]);
}

#[test]
fn temporaries_in_blocks_laid_out_apart_share_a_slot() {
    // Each temporary is written in a_i and read in c_i, which runs next but is laid out after
    // every a_j (as inlined code is): in the order control runs them the ranges are disjoint.
    let n = 8;
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let entry = fb.block();
    fb.assign(entry, sum, Rvalue::Use(int(0, Ty::U64)));
    let a: Vec<BlockId> = (0..n).map(|_| fb.block()).collect();
    let c: Vec<BlockId> = (0..n).map(|_| fb.block()).collect();
    let exit = fb.block();
    fb.term(entry, Terminator::Goto(a[0]));
    for i in 0..n {
        let t = fb.local(S);
        fb.assign(a[i], t, str_value());
        fb.term(a[i], Terminator::Goto(c[i]));
        fb.assign(
            c[i],
            sum,
            Rvalue::Binary(BinOp::Add, copy_local(sum), field(t, 0)),
        );
        fb.term(c[i], Terminator::Goto(*a.get(i + 1).unwrap_or(&exit)));
    }
    ret(exit, &mut fb);
    let f = fb.finish();
    let (_, count) = slots_of(&f, &memory(&f));
    assert_eq!(count, 1);
}

#[test]
fn liveness_work_is_bounded_in_a_long_loop() {
    // An extern's pointer argument is only read as far as the planner knows, so each result
    // is live all around the loop: a walk stops after WALK_BUDGET blocks and the local keeps
    // a slot of its own, instead of every walk visiting the whole loop.
    let n = 2000;
    let (_, visits) = plan_cost(&call_results(n, Callee::Extern(ExternId(0)), true));
    let bound = n as u64 * (u64::from(WALK_BUDGET) + 8);
    assert!(
        visits <= bound,
        "{visits} block visits for {n} results in a loop"
    );
}

#[test]
fn a_local_whose_walk_hits_the_budget_keeps_a_slot_of_its_own() {
    // bb0: a = …; goto head — head: read a — then a chain of more than WALK_BUDGET blocks with
    // u = …; read u in its middle, and a branch back to head. a is live all around the loop,
    // so its walk stops at the budget: it must still overlap u, and share its slot with no
    // other local (not even the unaccessed z).
    let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
    let sum = fb.local(Ty::U64);
    let cond = fb.local(Ty::Bool);
    let a = fb.local(S);
    let u = fb.local(S);
    let z = fb.local(S);
    let b0 = fb.block();
    let head = fb.block();
    fb.assign(b0, a, str_value());
    fb.assign(
        b0,
        cond,
        Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)),
    );
    fb.term(b0, Terminator::Goto(head));
    fb.assign(head, sum, Rvalue::Use(field(a, 0)));
    let n = WALK_BUDGET as usize + 100;
    let mut b = head;
    for i in 0..n {
        let next = fb.block();
        if i == n / 2 {
            fb.assign(next, u, str_value());
            fb.assign(next, sum, Rvalue::Use(field(u, 0)));
        }
        fb.term(b, Terminator::Goto(next));
        b = next;
    }
    let exit = fb.block();
    fb.term(
        b,
        Terminator::Branch {
            cond: copy_local(cond),
            then: head,
            els: exit,
        },
    );
    ret(exit, &mut fb);
    let f = fb.finish();
    let (slots, _) = slots_of(&f, &memory(&f));
    let own = slots[a.0 as usize];
    assert!(own.is_some());
    assert_ne!(own, slots[u.0 as usize], "a overlaps u");
    for (l, s) in slots.iter().enumerate() {
        assert!(l == a.0 as usize || *s != own, "local {l} shares a's slot");
    }
    assert!(slots[z.0 as usize].is_some());
}
