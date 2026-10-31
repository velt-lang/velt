//! Unit tests for testing a null pointee before calling a function that returns at once on it.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{FuncId, Linkage, Local};

/// The callee's entry block: its statements, given the parameter `p`, the read's local `t`, the
/// comparison's local `c` and a spare local `k` (`Ptr`).
type Entry = fn(p: Local, t: Local, c: Local, k: Local) -> Vec<Stmt>;

/// The usual entry: a constant into `k`, `t = *p`, `c = ne t, null`.
fn usual(p: Local, t: Local, c: Local, k: Local) -> Vec<Stmt> {
    vec![
        Stmt::Assign(Place::local(k), Rvalue::Use(int(16, Ty::Ptr))),
        read(t, deref(p, Ty::Ptr)),
        test(c, BinOp::Ne, t, int(0, Ty::Ptr)),
    ]
}

fn read(t: Local, from: Place) -> Stmt {
    Stmt::Assign(Place::local(t), Rvalue::Use(copy_place(from)))
}

fn test(c: Local, op: BinOp, t: Local, with: Operand) -> Stmt {
    Stmt::Assign(Place::local(c), bin(op, copy_local(t), with))
}

/// How the program is built: the callee's parameters and result, its entry, the comparison
/// (`Ne` branches to the work, `Eq` to the return), work on the way out, and how the caller
/// passes its argument.
struct Shape {
    params: Vec<Ty>,
    ret: Ty,
    entry: Entry,
    op: BinOp,
    exit_work: bool,
    arg: Arg,
    dest: bool,
}

#[derive(Clone, Copy)]
enum Arg {
    Local,
    Null,
    Field,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            params: vec![Ty::Ptr],
            ret: Ty::Unit,
            entry: usual,
            op: BinOp::Ne,
            exit_work: false,
            arg: Arg::Local,
            dest: false,
        }
    }
}

/// `drop(slot)`: `p = *slot; if p != null { free(p) }`, the shape of a drop glue, varied by
/// `s`, and `user(s)`, which calls it. Returns the program, `drop`'s and `user`'s ids.
fn program(s: &Shape) -> (Program, FuncId, usize) {
    let mut pb = ProgramBuilder::new();
    let free = pb.ext("velt_rt_free", &[Ty::Ptr], Ty::Unit, false);
    let mut fb = FuncBuilder::internal("drop", &s.params, s.ret);
    let (t, c, k) = (fb.local(Ty::Ptr), fb.local(Ty::Bool), fb.local(Ty::Ptr));
    let (entry, exit, body) = (fb.block(), fb.block(), fb.block());
    for st in (s.entry)(fb.param(0), t, c, k) {
        fb.push(entry, st);
    }
    match s.op {
        BinOp::Ne => fb.branch(entry, c, body, exit),
        _ => fb.branch(entry, c, exit, body),
    }
    let done = match s.ret {
        Ty::Unit => Operand::Const(Const::Unit, Ty::Unit),
        t => int(0, t),
    };
    if s.exit_work {
        let n = fb.call(
            exit,
            Callee::Extern(free),
            vec![copy_local(fb.param(0))],
            None,
        );
        fb.ret(n, done.clone());
    } else {
        fb.ret(exit, done.clone());
    }
    let n = fb.call(
        body,
        Callee::Extern(free),
        vec![copy_local(fb.param(0))],
        None,
    );
    fb.ret(n, done);
    let drop = pb.add(fb.finish());
    let mut fb = FuncBuilder::new("user", &[Ty::Ptr], Ty::Unit, Linkage::Export);
    let r = fb.local(s.ret);
    let b = fb.block();
    let arg = match s.arg {
        Arg::Local => copy_local(fb.param(0)),
        Arg::Null => int(0, Ty::Ptr),
        Arg::Field => copy_place(deref(fb.param(0), Ty::Ptr)),
    };
    let dest = s.dest.then_some(r);
    // Every further parameter gets the same pointer.
    let p0 = fb.param(0);
    let args = [vec![arg], vec![copy_local(p0); s.params.len() - 1]].concat();
    let n = fb.call(b, Callee::Func(drop), args, dest);
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    let user = pb.add(fb.finish());
    (pb.finish(), drop, user.0 as usize)
}

/// Whether `user`'s call of `drop` runs only where `*s` is not null, and calls `drop`'s clone
/// without the test: one return (after the work), where `drop` has two.
fn guarded(p: &Program, user: usize, drop: FuncId) -> bool {
    let f = &p.funcs[user];
    let Terminator::Branch { cond, then, els } = &f.blocks[0].term else {
        return false;
    };
    let reads = matches!(&f.blocks[0].stmts[..], [Stmt::Assign(t, Rvalue::Use(Operand::Copy(src))), Stmt::Assign(c, Rvalue::Binary(BinOp::Ne, Operand::Copy(t2), Operand::Const(Const::Int(0), Ty::Ptr)))]
        if *src == deref(Local(0), Ty::Ptr) && t == t2 && Operand::Copy(c.clone()) == *cond);
    let calls = matches!(&f.blocks[then.0 as usize].term, Terminator::Call { callee: Callee::Func(d), next, .. }
        if p.funcs[d.0 as usize].symbol == "drop$present" && next == els);
    let returns = |g: &Function| {
        g.blocks
            .iter()
            .filter(|b| matches!(b.term, Terminator::Return(_)))
            .count()
    };
    let clone = p.funcs.iter().find(|g| g.symbol == "drop$present");
    let one = clone.is_some_and(|g| returns(g) == 1) && returns(&p.funcs[drop.0 as usize]) == 2;
    reads && calls && one
}

/// Runs the pass on `user`; returns whether it changed anything, checking the call is still
/// the first block's terminator when it did not.
fn rewrites(s: &Shape) -> bool {
    let (mut p, drop, user) = program(s);
    let changed = run(&mut p);
    assert_valid(&p);
    match changed {
        true => assert!(guarded(&p, user, drop), "the call is behind a null test"),
        false => assert!(matches!(
            p.funcs[user].blocks[0].term,
            Terminator::Call { .. }
        )),
    }
    changed
}

#[test]
fn a_drop_of_a_null_slot_is_not_called() {
    assert!(rewrites(&Shape::default()));
    assert!(rewrites(&Shape {
        op: BinOp::Eq,
        entry: |p, t, c, k| {
            let mut s = usual(p, t, c, k);
            s[2] = test(c, BinOp::Eq, t, int(0, Ty::Ptr));
            s
        },
        ..Shape::default()
    }));
}

#[test]
fn a_callee_that_does_more_first_is_called_as_it_is() {
    // A store before the test, or work on the way out: skipping the call would skip it.
    assert!(!rewrites(&Shape {
        entry: |p, t, c, k| {
            let store = Stmt::Assign(deref(p, Ty::Ptr), Rvalue::Use(int(0, Ty::Ptr)));
            [vec![store], usual(p, t, c, k)].concat()
        },
        ..Shape::default()
    }));
    assert!(!rewrites(&Shape {
        exit_work: true,
        ..Shape::default()
    }));
}

#[test]
fn a_test_of_anything_but_the_pointee_read_first_is_left() {
    let cases: [Entry; 5] = [
        // A constant into the parameter before the read: the callee reads another pointer.
        |p, t, c, k| {
            [
                vec![Stmt::Assign(Place::local(p), Rvalue::Use(int(8, Ty::Ptr)))],
                usual(p, t, c, k),
            ]
            .concat()
        },
        // A constant into the read's local after it, or into the comparison's.
        |p, t, c, k| {
            [
                usual(p, t, c, k),
                vec![Stmt::Assign(Place::local(t), Rvalue::Use(int(0, Ty::Ptr)))],
            ]
            .concat()
        },
        |p, t, c, k| {
            [
                usual(p, t, c, k),
                vec![Stmt::Assign(Place::local(c), Rvalue::Use(boolean(true)))],
            ]
            .concat()
        },
        // A comparison with a pointer that is not null.
        |p, t, c, _| {
            vec![
                read(t, deref(p, Ty::Ptr)),
                test(c, BinOp::Ne, t, int(8, Ty::Ptr)),
            ]
        },
        // A read through a local that is not a parameter.
        |_, t, c, k| {
            vec![
                Stmt::Assign(Place::local(k), Rvalue::Use(int(16, Ty::Ptr))),
                read(t, deref(k, Ty::Ptr)),
                test(c, BinOp::Ne, t, int(0, Ty::Ptr)),
            ]
        },
    ];
    // The comparison before the read (into a second parameter): it tests the value before.
    assert!(!rewrites(&Shape {
        params: vec![Ty::Ptr, Ty::Ptr],
        entry: |p, _, c, _| {
            vec![
                test(c, BinOp::Ne, Local(1), int(0, Ty::Ptr)),
                read(Local(1), deref(p, Ty::Ptr)),
            ]
        },
        ..Shape::default()
    }));
    for (n, entry) in cases.into_iter().enumerate() {
        assert!(
            !rewrites(&Shape {
                entry,
                ..Shape::default()
            }),
            "case {n}"
        );
    }
}

#[test]
fn calls_that_do_not_fit_are_left() {
    // A constant or a projection as the argument; a callee with a result, which the call
    // writes.
    for arg in [Arg::Null, Arg::Field] {
        assert!(!rewrites(&Shape {
            arg,
            ..Shape::default()
        }));
    }
    assert!(!rewrites(&Shape {
        ret: Ty::I64,
        dest: true,
        ..Shape::default()
    }));
}
