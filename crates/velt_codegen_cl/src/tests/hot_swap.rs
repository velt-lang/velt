//! `DevSession::reload` (docs/internals/design/hot-reload.md, phase 3) on hand-built versions of one
//! program: which edits swap and which restart, and that code already running (the first
//! version's `velt_main`) reaches the new code through direct calls, function addresses and a
//! pinned state machine's entry.

use velt_vir::vir::Ty::*;

use super::jit::{call_capturing, stub_symbols};
use super::*;
use crate::{DevSession, Reload};

/// One version of the test program.
#[derive(Clone)]
struct Edit {
    /// What `answer` returns (plus `helper()`'s 100 when there is a helper).
    value: i64,
    /// `answer` takes an (unused) parameter: a signature change.
    param: bool,
    /// Fields of the `Point` layout `answer` uses.
    point_fields: u32,
    /// `velt_main` prints one more line: an edit of `main`.
    main_extra: bool,
    /// A new function `helper` that `answer` calls, inserted first (shifting every `FuncId`).
    helper: bool,
    /// What the pinned `task$poll` returns.
    poll: i64,
}

const V1: Edit = Edit {
    value: 1,
    param: false,
    point_fields: 1,
    main_extra: false,
    helper: false,
    poll: 7,
};

/// `answer`, `task` (returns `task$poll`'s address, like an async entry storing it in a future
/// header), `main`, which prints `answer()`, `(&answer)()` and `task()(0, 0)`, and
/// `velt_main`.
fn program(e: &Edit) -> Program {
    let (mut pb, rt) = ProgramBuilder::new();
    let helper = e.helper.then(|| pb.add(constant_fn("helper", 100)));
    let point = pb.agg(AggLayout {
        name: "Point".into(),
        size: 8 * e.point_fields,
        align: 8,
        fields: (0..e.point_fields).map(|i| (I64, 8 * i)).collect(),
    });
    let answer = pb.add(answer_fn(e, point, helper));
    let poll = pb.add(poll_fn(e.poll));
    let task = pb.add(task_fn(poll));
    let main = pb.add(main_fn(e, &rt, answer, task));
    pb.add(entry_fn(main));
    pb.p
}

fn constant_fn(symbol: &str, value: i64) -> Function {
    let mut fb = FuncBuilder::internal(symbol, &[], I64);
    let b = fb.block();
    fb.term(b, Terminator::Return(int(value.into(), I64)));
    fb.finish()
}

fn answer_fn(e: &Edit, point: AggId, helper: Option<FuncId>) -> Function {
    let params: &[Ty] = if e.param { &[I64] } else { &[] };
    let mut fb = FuncBuilder::internal("answer", params, I64);
    let pt = fb.local(Agg(point));
    let r = fb.local(I64);
    let b = fb.block();
    let field = place(pt, vec![Proj::Field(0)]);
    fb.push(
        b,
        Stmt::Assign(field.clone(), Rvalue::Use(int(e.value.into(), I64))),
    );
    fb.assign(b, r, Rvalue::Use(copy_place(field)));
    let b = match helper {
        Some(h) => {
            let got = fb.local(I64);
            let next = fb.call(b, Callee::Func(h), vec![], Some(Place::local(got)));
            let sum = Rvalue::Binary(BinOp::Add, copy_local(r), copy_local(got));
            fb.assign(next, r, sum);
            next
        }
        None => b,
    };
    fb.term(b, Terminator::Return(copy_local(r)));
    fb.finish()
}

fn poll_fn(value: i64) -> Function {
    let mut fb = FuncBuilder::internal("task$poll", &[Ptr, Ptr], U32);
    let b = fb.block();
    fb.term(b, Terminator::Return(int(value.into(), U32)));
    fb.finish()
}

fn task_fn(poll: FuncId) -> Function {
    let mut fb = FuncBuilder::internal("task", &[], Ptr);
    let b = fb.block();
    let address = Operand::Const(Const::Func(poll), Ptr);
    fb.term(b, Terminator::Return(address));
    fb.finish()
}

fn main_fn(e: &Edit, rt: &Rt, answer: FuncId, task: FuncId) -> Function {
    let mut fb = FuncBuilder::internal("main", &[], Unit);
    let b = fb.block();
    let args = if e.param { vec![int(0, I64)] } else { vec![] };
    let (direct, pointer, f, state) = (fb.local(I64), fb.local(I64), fb.local(Ptr), fb.local(U32));
    let mut cur = fb.call(
        b,
        Callee::Func(answer),
        args.clone(),
        Some(Place::local(direct)),
    );
    fb.assign(
        cur,
        f,
        Rvalue::Use(Operand::Const(Const::Func(answer), Ptr)),
    );
    let params = if e.param { vec![I64] } else { vec![] };
    let callee = Callee::Ptr {
        target: copy_local(f),
        params,
        ret: I64,
    };
    cur = fb.call(cur, callee, args, Some(Place::local(pointer)));
    cur = fb.call(cur, Callee::Func(task), vec![], Some(Place::local(f)));
    let poll = Callee::Ptr {
        target: copy_local(f),
        params: vec![Ptr, Ptr],
        ret: U32,
    };
    let null = || int(0, Ptr);
    cur = fb.call(cur, poll, vec![null(), null()], Some(Place::local(state)));
    let mut out = Out {
        fb: &mut fb,
        rt,
        cur,
    };
    out.line(copy_local(direct), I64);
    out.line(copy_local(pointer), I64);
    out.line(copy_local(state), U32);
    if e.main_extra {
        out.line(int(0, I64), I64);
    }
    let cur = out.cur;
    fb.term(cur, Terminator::Return(Operand::Const(Const::Unit, Unit)));
    fb.finish()
}

/// `velt_main`: calls the user `main`.
fn entry_fn(main: FuncId) -> Function {
    let (mut fb, b) = main_fb();
    let cur = fb.call(b, Callee::Func(main), vec![], None);
    finish_main(fb, cur, 0)
}

/// Load `V1`, check its output, then reload `edit`: the outcome and, after a swap, what the
/// first version's `main` prints now.
fn reload(edit: Edit) -> (Reload, String) {
    let mut session = DevSession::new(&stub_symbols());
    let loaded = session.load(&program(&V1)).expect("load v1");
    assert_eq!(call_capturing(loaded.main()), (0, "1\n1\n7\n".to_string()));
    let outcome = session.reload(&program(&edit)).expect("reload");
    let (_, stdout) = call_capturing(loaded.main());
    (outcome, stdout)
}

fn swapped(functions: usize) -> Reload {
    Reload::Swapped { functions }
}

#[test]
fn a_body_edit_swaps_for_calls_and_function_values() {
    let edit = Edit { value: 2, ..V1 };
    assert_eq!(reload(edit), (swapped(1), "2\n2\n7\n".into()));
}

#[test]
fn a_new_function_swaps_in_even_when_every_id_shifts() {
    let edit = Edit { helper: true, ..V1 };
    assert_eq!(reload(edit), (swapped(2), "101\n101\n7\n".into()));
}

#[test]
fn a_changed_pinned_function_recompiles_its_users() {
    let edit = Edit { poll: 8, ..V1 };
    assert_eq!(reload(edit), (swapped(2), "1\n1\n8\n".into()));
}

#[test]
fn an_unchanged_program_swaps_nothing() {
    assert_eq!(reload(V1), (swapped(0), "1\n1\n7\n".into()));
}

#[test]
fn restarts_say_why() {
    let cases = [
        (
            Edit { param: true, ..V1 },
            "the signature of answer changed",
        ),
        (
            Edit {
                point_fields: 2,
                ..V1
            },
            "Point gained a field",
        ),
        (
            Edit {
                main_extra: true,
                ..V1
            },
            "main changed (it already ran)",
        ),
    ];
    for (edit, reason) in cases {
        let (outcome, stdout) = reload(edit);
        assert_eq!(outcome, Reload::Restart(reason.into()));
        assert_eq!(
            stdout, "1\n1\n7\n",
            "a restart decision leaves the code alone"
        );
    }
}

#[test]
fn repeated_swaps_keep_redirecting_old_code() {
    let mut session = DevSession::new(&stub_symbols());
    let loaded = session.load(&program(&V1)).expect("load v1");
    for value in 2..6 {
        let edit = Edit { value, ..V1 };
        assert_eq!(session.reload(&program(&edit)), Ok(swapped(1)));
        let want = format!("{value}\n{value}\n7\n");
        assert_eq!(call_capturing(loaded.main()), (0, want));
    }
}

/// Version `generation` of a program whose `velt_main` returns `run()`, which returns
/// `answer()`: `answer` calls
/// `helper_<generation>`, a function new in this version (its own new slot and trampoline),
/// which returns `generation`. Earlier helpers stay, so every version is a swap.
fn growing(generation: i64) -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    // `answer` comes before the helper it newly calls: its slot is set first.
    let answer = pb.reserve();
    let helpers: Vec<FuncId> = (1..=generation)
        .map(|g| pb.add(constant_fn(&format!("helper_{g}"), g)))
        .collect();
    let mut fb = FuncBuilder::internal("answer", &[], I64);
    let r = fb.local(I64);
    let b = fb.block();
    let next = fb.call(
        b,
        Callee::Func(helpers[helpers.len() - 1]),
        vec![],
        Some(Place::local(r)),
    );
    fb.term(next, Terminator::Return(copy_local(r)));
    pb.set(answer, fb.finish());
    // `velt_main` and what it calls count as `main` (never swapped): `run` keeps `answer` out.
    let mut fb = FuncBuilder::internal("run", &[], I64);
    let r = fb.local(I64);
    let b = fb.block();
    let next = fb.call(b, Callee::Func(answer), vec![], Some(Place::local(r)));
    fb.term(next, Terminator::Return(copy_local(r)));
    let run = pb.add(fb.finish());
    let (mut fb, b) = main_fb();
    let got = fb.local(I64);
    let next = fb.call(b, Callee::Func(run), vec![], Some(Place::local(got)));
    let code = Rvalue::Cast(copy_local(got), I32);
    let ret = fb.local(I32);
    fb.assign(next, ret, code);
    fb.term(next, Terminator::Return(copy_local(ret)));
    pb.add(fb.finish());
    pb.p
}

/// Swaps while other threads keep calling the running code (#880): every call runs whole
/// code of some version (no fault, no torn state), and a call that starts after a swap
/// returned runs that version or a newer one. Ordered by what the callers observe: each
/// reads the published generation before its call.
#[test]
fn swaps_under_concurrent_calls() {
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
    const GENERATIONS: i64 = 120;
    const CALLERS: usize = 4;
    let mut session = DevSession::new(&stub_symbols());
    let main = session.load(&growing(1)).expect("load").main();
    let published = AtomicI64::new(1);
    let done = AtomicBool::new(false);
    let calling = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let callers: Vec<_> = (0..CALLERS)
            .map(|_| {
                s.spawn(|| {
                    let mut calls = 0u64;
                    while !done.load(Ordering::Acquire) {
                        let at_least = published.load(Ordering::Acquire);
                        let got = i64::from(main());
                        assert!(
                            (at_least..=GENERATIONS).contains(&got),
                            "a call after swap {at_least} returned {got}"
                        );
                        if calls == 0 {
                            calling.fetch_add(1, Ordering::AcqRel);
                        }
                        calls += 1;
                    }
                })
            })
            .collect();
        /// Stops the callers however the swaps end (a failed assertion included).
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let stop = Stop(&done);
        // Swap only once every caller is calling.
        while calling.load(Ordering::Acquire) < CALLERS {
            std::thread::yield_now();
        }
        for generation in 2..=GENERATIONS {
            let outcome = session.reload(&growing(generation));
            assert_eq!(outcome, Ok(swapped(2)), "generation {generation}");
            published.store(generation, Ordering::Release);
        }
        drop(stop);
        for caller in callers {
            caller.join().expect("caller");
        }
    });
    assert_eq!(i64::from(main()), GENERATIONS);
}
