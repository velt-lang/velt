//! Slot sharing (`slots::plan`, #2048): locals whose live ranges are disjoint share a slot;
//! overlapping, escaped and described locals do not.

use velt_vir::vir::{
    self, BinOp, BlockId, Callee, Const, DebugTyId, ExternFn, ExternId, Local, LocalDebug,
    LocalDecl, Operand, Place, Program, Proj, Rvalue, SrcLoc, Stmt, Terminator, Ty, STR_AGG,
};

use super::slots::plan;
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
            },
            // 1: keep(ptr) -> ptr (returns its argument)
            ExternFn {
                symbol: "keep".into(),
                params: vec![Ty::Ptr],
                ret: Ty::Ptr,
                noreturn: false,
                host: None,
            },
        ],
        ..Default::default()
    }
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
    f.locals.iter().map(|l| matches!(l.ty, Ty::Agg(_))).collect()
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
    fb.assign(b0, cond, Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)));
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
    fb.assign(b0, cond, Rvalue::Use(Operand::Const(Const::Bool(true), Ty::Bool)));
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
    let next = fb.call(
        b,
        Callee::Extern(ExternId(0)),
        vec![copy_local(p)],
        None,
    );
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
    assert!(slots[a.0 as usize] == slots[bl.0 as usize] && slots[bl.0 as usize] == slots[c.0 as usize]);
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
    let p = plan(&program(), &f, &memory(&f), &|i| f.locals[i].debug.is_some()).unwrap();
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
