//! Module state across `DevSession::reload` (#2654): the runtime keys the block by the address
//! of the program's descriptor, so a swapped-in version that keeps the module state's key
//! imports the running descriptor (its code reaches the same block), and one that changes it
//! moves every function reaching module state, changed or not, to a descriptor of its own.

use std::cell::RefCell;

use velt_vir::vir::Ty::*;

use super::jit::stub_symbols;
use super::*;
use crate::{DevSession, Reload};

thread_local! {
    /// The descriptors `velt_rt_module_state` was called with, in order.
    static DESCS: RefCell<Vec<usize>> = const { RefCell::new(vec![]) };
    /// The block every call returns, one per test thread: the test programs never read it.
    static SLOT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

extern "C" fn module_state(desc: *const u8, _slot: u64) -> *mut u8 {
    DESCS.with(|d| d.borrow_mut().push(desc as usize));
    SLOT.with(|s| s.as_ptr().cast())
}

/// One version: what `read` returns (an edit of a function using module state) and what the
/// state's `init` stores (an edit of an initializer).
#[derive(Clone, Copy)]
struct Edit {
    read: i64,
    init: i64,
}

/// `init` (the block's initializer) and `drop`, the descriptor pointing at them, `other` and
/// `read`, which reach the block, `run`, which calls both, and `velt_main`, which calls `run`.
fn program(e: Edit) -> Program {
    program_in(e, false)
}

/// `program`, its users reaching the block as under the executor in Velt (`VELT_SCHEDULER=std`)
/// when `std_sched`: through the std hook `schedModuleState(desc as usize, slot)` (lowered to a
/// function of the program), which here passes them on to `velt_rt_module_state`.
fn program_in(e: Edit, std_sched: bool) -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    let state = pb.ext("velt_rt_module_state", &[Ptr, U64], Ptr, false);
    let hook = std_sched.then(|| {
        let symbol = velt_vir::mangle::mangle(velt_common::std_hook!(sched_module_state));
        let mut fb = FuncBuilder::internal(&symbol, &[U64, U64], U64);
        let (p, r, out) = (fb.local(Ptr), fb.local(Ptr), fb.local(U64));
        let b = fb.block();
        fb.assign(b, p, Rvalue::Cast(copy_local(fb.param(0)), Ptr));
        let args = vec![copy_local(p), copy_local(fb.param(1))];
        let next = fb.call(b, Callee::Extern(state), args, Some(Place::local(r)));
        fb.assign(next, out, Rvalue::Cast(copy_local(r), U64));
        fb.term(next, Terminator::Return(copy_local(out)));
        pb.add(fb.finish())
    });
    let mut fb = FuncBuilder::internal("module_init", &[Ptr], Unit);
    let b = fb.block();
    let slot = place(fb.param(0), vec![Proj::Deref(I64)]);
    fb.push(b, Stmt::Assign(slot, Rvalue::Use(int(e.init.into(), I64))));
    fb.term(b, Terminator::Return(Operand::Const(Const::Unit, Unit)));
    let init = pb.add(fb.finish());
    let mut fb = FuncBuilder::internal("module_drop", &[Ptr], Unit);
    let b = fb.block();
    fb.term(b, Terminator::Return(Operand::Const(Const::Unit, Unit)));
    let drop = pb.add(fb.finish());
    let mut bytes = vec![0u8; 48];
    bytes[0..8].copy_from_slice(&8u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&8u64.to_le_bytes());
    let relocs = vec![(16, Const::Func(init)), (24, Const::Func(drop))];
    let desc = pb.stat_with(&bytes, 8, relocs);
    let user = |name: &str, value: i64| {
        let mut fb = FuncBuilder::internal(name, &[], I64);
        let p = fb.local(Ptr);
        let b = fb.block();
        let next = match hook {
            Some(hook) => {
                let (d, r) = (fb.local(U64), fb.local(U64));
                let addr = Operand::Const(Const::Static(desc), Ptr);
                fb.assign(b, d, Rvalue::Cast(addr, U64));
                let args = vec![copy_local(d), int(0, U64)];
                fb.call(b, Callee::Func(hook), args, Some(Place::local(r)))
            }
            None => {
                let args = vec![Operand::Const(Const::Static(desc), Ptr), int(0, U64)];
                fb.call(b, Callee::Extern(state), args, Some(Place::local(p)))
            }
        };
        fb.term(next, Terminator::Return(int(value.into(), I64)));
        fb.finish()
    };
    let other = pb.add(user("other", 0));
    let read = pb.add(user("read", e.read));
    let mut fb = FuncBuilder::internal("run", &[], I64);
    let (r, ignored) = (fb.local(I64), fb.local(I64));
    let b = fb.block();
    let next = fb.call(b, Callee::Func(other), vec![], Some(Place::local(ignored)));
    let next = fb.call(next, Callee::Func(read), vec![], Some(Place::local(r)));
    fb.term(next, Terminator::Return(copy_local(r)));
    let run = pb.add(fb.finish());
    let (mut fb, b) = main_fb();
    let got = fb.local(I64);
    let ret = fb.local(I32);
    let next = fb.call(b, Callee::Func(run), vec![], Some(Place::local(got)));
    fb.assign(next, ret, Rvalue::Cast(copy_local(got), I32));
    fb.term(next, Terminator::Return(copy_local(ret)));
    pb.add(fb.finish());
    pb.p
}

/// What `velt_main` returns and the descriptors `other` and `read` passed.
fn run(main: extern "C" fn() -> i32) -> (i32, Vec<usize>) {
    DESCS.with(|d| d.borrow_mut().clear());
    let rc = main();
    (rc, DESCS.with(|d| std::mem::take(&mut *d.borrow_mut())))
}

fn session() -> DevSession {
    let mut symbols = stub_symbols().to_vec();
    symbols.push(("velt_rt_module_state", module_state as *const u8));
    DevSession::new(&symbols)
}

const V1: Edit = Edit { read: 1, init: 5 };

#[test]
fn a_swap_that_keeps_the_module_state_keeps_its_block() {
    let mut session = session();
    let main = session.load(&program(V1)).expect("load").main();
    let (rc, descs) = run(main);
    assert_eq!(rc, 1);
    let first = descs[0];
    assert_eq!(descs, [first, first]);
    let edit = Edit { read: 2, ..V1 };
    let outcome = session.reload(&program(edit));
    assert_eq!(outcome, Ok(Reload::Swapped { functions: 1 }));
    assert_eq!(
        run(main),
        (2, vec![first, first]),
        "the swapped `read` reaches the running block"
    );
    let edit = Edit { read: 3, ..V1 };
    assert_eq!(
        session.reload(&program(edit)),
        Ok(Reload::Swapped { functions: 1 })
    );
    assert_eq!(run(main), (3, vec![first, first]), "and after another swap");
}

/// Under the executor in Velt, module state goes through std hooks, which take the descriptor cast
/// to an integer: a swap still sees the running version's block and keeps it (the reload suite's
/// `module_state_kept` lost its state there, P2i of #2649).
#[test]
fn a_swap_keeps_the_block_reached_through_the_std_schedulers_hooks() {
    let mut session = session();
    let main = session.load(&program_in(V1, true)).expect("load").main();
    let (rc, descs) = run(main);
    assert_eq!(rc, 1);
    let first = descs[0];
    assert_eq!(descs, [first, first]);
    let edit = Edit { read: 2, ..V1 };
    let outcome = session.reload(&program_in(edit, true));
    assert_eq!(outcome, Ok(Reload::Swapped { functions: 1 }));
    assert_eq!(
        run(main),
        (2, vec![first, first]),
        "the swapped `read` reaches the running block"
    );
    // An initializer changed: every user moves to a block of the new version's own.
    let edit = Edit { read: 2, init: 6 };
    assert_eq!(
        session.reload(&program_in(edit, true)),
        Ok(Reload::Swapped { functions: 3 })
    );
    let (_, descs) = run(main);
    assert_ne!(descs[0], first);
    assert_eq!(descs, [descs[0], descs[0]]);
}

#[test]
fn a_swap_that_changes_an_initializer_moves_every_user_to_a_new_block() {
    let mut session = session();
    let main = session.load(&program(V1)).expect("load").main();
    let (_, descs) = run(main);
    let first = descs[0];
    let edit = Edit { init: 6, ..V1 };
    // `init`, and both users of the block although neither changed.
    let outcome = session.reload(&program(edit));
    assert_eq!(outcome, Ok(Reload::Swapped { functions: 3 }));
    let (rc, descs) = run(main);
    assert_eq!(rc, 1);
    assert_ne!(descs[0], first, "a block of the new version's own");
    assert_eq!(descs, [descs[0], descs[0]], "unchanged code moved with it");
    // The new descriptor keys the block from now on.
    let second = descs[0];
    let edit = Edit { read: 4, init: 6 };
    assert_eq!(
        session.reload(&program(edit)),
        Ok(Reload::Swapped { functions: 1 })
    );
    assert_eq!(run(main), (4, vec![second, second]));
}

/// A swap switches the slots of the new descriptor's functions before the other existing ones,
/// so that no request can make the new block while its `init` slot still holds the old code
/// (#2811). New keys still come first (#880).
#[test]
fn the_new_descriptors_functions_are_switched_before_the_code_reaching_the_block() {
    // 0, 2, 3: existing keys; 1, 4: new ones; 3: the descriptor's `init`.
    let trampolines = [None, Some(1), None, None, Some(2)];
    assert_eq!(crate::dev::store_order(&trampolines, &[3]), [1, 4, 3, 0, 2]);
    // A descriptor function with a new key is switched with the new keys, once.
    assert_eq!(
        crate::dev::store_order(&trampolines, &[4, 3]),
        [1, 4, 3, 0, 2]
    );
    assert_eq!(crate::dev::store_order(&trampolines, &[]), [1, 4, 0, 2, 3]);
}
