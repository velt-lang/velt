//! Unit tests for caching module state lookups on hand-built VIR: each program runs in the
//! interpreter before and after the pass, with a host that counts the lookups and stops at a
//! slot that is not initialized yet.

use super::{run, Lookups};
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{
    BinOp, BlockId, Callee, Const, ExternFn, ExternId, Local, Operand, Place, Program, Rvalue,
    StaticId, Stmt,
};

/// The module state block: 32 bytes made on the first lookup; slots below `ready` may be read.
struct Host {
    ready: u64,
    block: Option<u64>,
    lookups: usize,
}

impl crate::interp::Host for Host {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_module_state" => {
                self.lookups += 1;
                if args[1] >= self.ready {
                    return Err(Trap::NoReturn("ReferenceError".into()));
                }
                match self.block {
                    Some(p) => Ok(p),
                    None => {
                        let p = mem.alloc_heap(32, 8)?;
                        mem.write(p, &[0; 32])?;
                        self.block = Some(p);
                        Ok(p)
                    }
                }
            }
            _ => Ok(0),
        }
    }
}

/// `main(n)`'s result and how many lookups it made, or the trap.
fn run_main(p: &Program, n: u64, ready: u64) -> Result<(u64, usize), Trap> {
    let host = Host {
        ready,
        block: None,
        lookups: 0,
    };
    let mut interp = Interp::new(p, host);
    let r = interp.call_symbol("main", &[n])?;
    Ok((r, interp.host.lookups))
}

struct Env {
    pb: ProgramBuilder,
    state: ExternId,
    block_on: ExternId,
    desc: StaticId,
}

fn env() -> Env {
    let mut pb = ProgramBuilder::new();
    let state = pb.ext("velt_rt_module_state", &[Ptr, U64], Ptr, false);
    let block_on = pb.ext("velt_rt_block_on", &[Ptr, Ptr], Unit, false);
    let desc = pb.stat(&[0; 48], 8);
    Env {
        pb,
        state,
        block_on,
        desc,
    }
}

impl Env {
    /// `p = velt_rt_module_state(&desc, slot)`; returns the block after the call.
    fn lookup(&self, fb: &mut FuncBuilder, b: BlockId, slot: i128, p: Local) -> BlockId {
        let args = vec![
            Operand::Const(Const::Static(self.desc), Ptr),
            int(slot, U64),
        ];
        fb.call(b, Callee::Extern(self.state), args, Some(p))
    }

    /// Slot `slot` of the block `p` points to (an `i64` at `8 * slot`).
    fn slot(fb: &mut FuncBuilder, b: BlockId, p: Local, slot: i128) -> Place {
        if slot == 0 {
            return deref(p, I64);
        }
        let q = fb.local(Ptr);
        fb.assign(b, q, bin(BinOp::PtrAdd, copy_local(p), int(8 * slot, I64)));
        deref(q, I64)
    }

    fn program(mut self, fb: FuncBuilder) -> Program {
        self.pb.add(fb.finish());
        self.pb.finish()
    }
}

/// `for (i = 0; i < n; i++) s[slot] += i; return s[slot]`, a lookup per access.
fn loop_program(slot: i128) -> Program {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let n = fb.param(0);
    let (i, c, p, v, q, r) = (
        fb.local(I64),
        fb.local(Bool),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(Ptr),
        fb.local(I64),
    );
    let entry = fb.block();
    let (head, body, exit) = (fb.block(), fb.block(), fb.block());
    fb.assign(entry, i, Rvalue::Use(int(0, I64)));
    fb.goto(entry, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    let b = env.lookup(&mut fb, body, slot, p);
    let s = Env::slot(&mut fb, b, p, slot);
    fb.assign(b, v, bin(BinOp::Add, copy_place(s.clone()), copy_local(i)));
    fb.push(b, Stmt::Assign(s, Rvalue::Use(copy_local(v))));
    fb.assign(b, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(b, head);
    let b = env.lookup(&mut fb, exit, slot, q);
    let s = Env::slot(&mut fb, b, q, slot);
    fb.push(b, Stmt::Assign(Place::local(r), Rvalue::Use(copy_place(s))));
    fb.ret(b, copy_local(r));
    env.program(fb)
}

/// Run the pass on `main`; check that the program stays valid and computes the same.
fn optimized(original: &Program, expect_changed: bool) -> Program {
    let mut p = original.clone();
    let lookups = Lookups::find(&p);
    assert_eq!(run(lookups, &mut p.funcs[0]), expect_changed, "{p}");
    assert_valid(&p);
    p
}

#[test]
fn a_loop_asks_the_runtime_once() {
    let original = loop_program(1);
    let p = optimized(&original, true);
    for n in [0, 1, 7] {
        let (before, asked_before) = run_main(&original, n, u64::MAX).expect("runs");
        let (after, asked_after) = run_main(&p, n, u64::MAX).expect("runs");
        assert_eq!(before, after, "n = {n}");
        assert_eq!(asked_before, n as usize + 1, "n = {n}");
        assert_eq!(asked_after, 1, "n = {n}\n{p}");
    }
}

#[test]
fn an_uninitialized_slot_still_stops_at_its_first_access() {
    let original = loop_program(2);
    let p = optimized(&original, true);
    for n in [0, 3] {
        assert!(matches!(run_main(&original, n, 2), Err(Trap::NoReturn(_))));
        assert!(matches!(run_main(&p, n, 2), Err(Trap::NoReturn(_))));
    }
}

/// `return s[first] + s[second]`, a lookup each.
fn two_slots(first: i128, second: i128) -> Program {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (p, q, a, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(I64), fb.local(I64));
    let b = fb.block();
    let b = env.lookup(&mut fb, b, first, p);
    let s = Env::slot(&mut fb, b, p, first);
    fb.push(b, Stmt::Assign(Place::local(a), Rvalue::Use(copy_place(s))));
    let b = env.lookup(&mut fb, b, second, q);
    let s = Env::slot(&mut fb, b, q, second);
    fb.assign(b, r, bin(BinOp::Add, copy_local(a), copy_place(s)));
    fb.ret(b, copy_local(r));
    env.program(fb)
}

#[test]
fn a_lower_slot_after_a_higher_one_is_not_asked_again() {
    let original = two_slots(3, 1);
    let p = optimized(&original, true);
    assert_eq!(run_main(&original, 0, u64::MAX).expect("runs").1, 2);
    assert_eq!(run_main(&p, 0, u64::MAX).expect("runs").1, 1, "{p}");
}

#[test]
fn a_higher_slot_after_a_lower_one_is_checked() {
    let original = two_slots(1, 3);
    let p = optimized(&original, true);
    assert_eq!(run_main(&p, 0, u64::MAX).expect("runs").1, 2, "{p}");
    // Slot 3 is not initialized: the second access still stops.
    assert!(matches!(run_main(&p, 0, 2), Err(Trap::NoReturn(_))));
}

#[test]
fn one_lookup_outside_a_loop_is_left_alone() {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (p, r) = (fb.local(Ptr), fb.local(I64));
    let b = fb.block();
    let b = env.lookup(&mut fb, b, 0, p);
    fb.push(
        b,
        Stmt::Assign(Place::local(r), Rvalue::Use(copy_place(deref(p, I64)))),
    );
    fb.ret(b, copy_local(r));
    optimized(&env.program(fb), false);
}

#[test]
fn a_function_that_hands_the_lock_to_the_tasks_is_left_alone() {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (p, q, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(I64));
    let b = fb.block();
    let b = env.lookup(&mut fb, b, 0, p);
    let null = || Operand::Const(Const::Int(0), Ptr);
    let b = fb.call(b, Callee::Extern(env.block_on), vec![null(), null()], None);
    let b = env.lookup(&mut fb, b, 0, q);
    fb.push(
        b,
        Stmt::Assign(Place::local(r), Rvalue::Use(copy_place(deref(q, I64)))),
    );
    fb.ret(b, copy_local(r));
    optimized(&env.program(fb), false);
}
