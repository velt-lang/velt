//! Tests for `arrays`: hand-built arrays of the shape lowering and `sroa` leave (a data
//! pointer, `ptradd` to an element, `f64` loads and stores), each run in the reference
//! interpreter before and after `numrep`.

use super::*;
use crate::interp::{Host, Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, BlockId, ExternFn, Place, Program, Proj, Stmt, Terminator};

/// The allocator, and an opaque `keep(ptr)` and `num() -> f64`.
struct HeapHost;

impl Host for HeapHost {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_alloc" => mem.alloc_heap(args[0], args[1]),
            "velt_rt_free" | "keep" => Ok(0),
            "num" => Ok(0.5f64.to_bits()),
            s => Err(Trap::Invalid(format!("unexpected call to {s}"))),
        }
    }
}

/// `f(x)` in the interpreter, as an `f64`.
fn run(p: &Program, x: f64) -> f64 {
    let mut i = Interp::new(p, HeapHost);
    i.set_fuel(100_000);
    f64::from_bits(i.call_symbol("f", &[x.to_bits()]).expect("program runs"))
}

/// The element accesses of `f` with type `ty`.
fn derefs(p: &Program, ty: Ty) -> usize {
    let f = &p.funcs[0];
    let mut n = 0;
    let mut count = |pl: &Place| n += usize::from(pl.proj == [Proj::Deref(ty)]);
    for b in &f.blocks {
        for s in &b.stmts {
            crate::visit::stmt_places(s, &mut |pl, _| count(pl));
        }
        crate::visit::term_places(&b.term, &mut |pl, _| count(pl));
    }
    n
}

fn elem(p: Local) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::F64)],
    }
}

/// What the test array does besides filling and swapping.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    None,
    /// Stores 0.5 into element 1.
    Fraction,
    /// Stores -0 into element 1.
    NegZero,
    /// Hands the data pointer to `keep`.
    Escapes,
    /// Adds 1 to element 1 on every turn of a loop that has no bound on the value.
    Grows,
    /// Stores `num()` into element 1 as a call destination.
    CallResult,
}

/// `f(x)`: four elements `xs[i] = i` (an `i64` counter), `xs[0]` and `xs[3]` swapped, the
/// `extra`, then `xs[0] + xs[1] + x`; the buffer freed.
fn program(extra: Extra) -> Program {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[Ty::U64, Ty::U64], Ty::Ptr, false);
    let free = pb.ext(
        "velt_rt_free",
        &[Ty::Ptr, Ty::U64, Ty::U64],
        Ty::Unit,
        false,
    );
    let keep = pb.ext("keep", &[Ty::Ptr], Ty::Unit, false);
    let num = pb.ext("num", &[], Ty::F64, false);
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let x = fb.param(0);
    let (p, i, c, off, q, v, j) = (
        fb.local(Ty::Ptr),
        fb.local(Ty::I64),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::Ptr),
        fb.local(Ty::F64),
        fb.local(Ty::Ptr),
    );
    let (t, a, b, r, s) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let entry = fb.block();
    let head = fb.call(
        entry,
        Callee::Extern(alloc),
        vec![int(32, Ty::U64), int(8, Ty::U64)],
        Some(p),
    );
    let (body, after) = (fb.block(), fb.block());
    fb.assign(head, i, Rvalue::Use(int(0, Ty::I64)));
    let lh = fb.block();
    fb.goto(head, lh);
    fb.assign(lh, c, bin(BinOp::Lt, copy_local(i), int(4, Ty::I64)));
    fb.branch(lh, c, body, after);
    fb.assign(body, off, bin(BinOp::Mul, copy_local(i), int(8, Ty::I64)));
    fb.assign(body, q, bin(BinOp::PtrAdd, copy_local(p), copy_local(off)));
    fb.assign(body, v, Rvalue::Cast(copy_local(i), Ty::F64));
    fb.push(body, Stmt::Assign(elem(q), Rvalue::Use(copy_local(v))));
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
    fb.goto(body, lh);
    // Swap xs[0] and xs[3].
    fb.assign(
        after,
        j,
        bin(BinOp::PtrAdd, copy_local(p), int(24, Ty::I64)),
    );
    fb.assign(after, t, Rvalue::Use(copy_place(elem(p))));
    fb.push(
        after,
        Stmt::Assign(elem(p), Rvalue::Use(copy_place(elem(j)))),
    );
    fb.push(after, Stmt::Assign(elem(j), Rvalue::Use(copy_local(t))));
    let one = fb.local(Ty::Ptr);
    fb.assign(
        after,
        one,
        bin(BinOp::PtrAdd, copy_local(p), int(8, Ty::I64)),
    );
    let mut cont: BlockId = after;
    match extra {
        Extra::None => {}
        Extra::Fraction => fb.push(
            cont,
            Stmt::Assign(elem(one), Rvalue::Use(float(0.5, Ty::F64))),
        ),
        Extra::NegZero => fb.push(
            cont,
            Stmt::Assign(elem(one), Rvalue::Use(float(-0.0, Ty::F64))),
        ),
        Extra::Escapes => cont = fb.call(cont, Callee::Extern(keep), vec![copy_local(p)], None),
        Extra::CallResult => {
            let next = fb.block();
            let call = Terminator::Call {
                callee: Callee::Extern(num),
                args: vec![],
                dest: Some(elem(one)),
                next,
            };
            fb.term(cont, call);
            cont = next;
        }
        Extra::Grows => {
            let (k, kc, e) = (fb.local(Ty::I64), fb.local(Ty::Bool), fb.local(Ty::F64));
            let (gh, gb, gx) = (fb.block(), fb.block(), fb.block());
            fb.assign(cont, k, Rvalue::Use(int(0, Ty::I64)));
            fb.goto(cont, gh);
            fb.assign(gh, kc, bin(BinOp::Lt, copy_local(k), copy_local(i)));
            fb.branch(gh, kc, gb, gx);
            fb.assign(
                gb,
                e,
                bin(BinOp::Add, copy_place(elem(one)), float(1.0, Ty::F64)),
            );
            fb.push(gb, Stmt::Assign(elem(one), Rvalue::Use(copy_local(e))));
            fb.assign(gb, k, bin(BinOp::Add, copy_local(k), int(1, Ty::I64)));
            fb.goto(gb, gh);
            cont = gx;
        }
    }
    fb.assign(cont, a, Rvalue::Use(copy_place(elem(p))));
    fb.assign(cont, b, Rvalue::Use(copy_place(elem(one))));
    fb.assign(cont, s, bin(BinOp::Add, copy_local(a), copy_local(b)));
    fb.assign(cont, r, bin(BinOp::Add, copy_local(s), copy_local(x)));
    let done = fb.call(
        cont,
        Callee::Extern(free),
        vec![copy_local(p), int(32, Ty::U64), int(8, Ty::U64)],
        None,
    );
    fb.ret(done, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

/// Run `numrep` on `program(extra)`; check validity and that `f` computes the same.
fn narrowed(extra: Extra) -> (Program, Program) {
    let p = program(extra);
    let mut q = p.clone();
    run_program(&mut q);
    assert_valid(&q);
    for x in [0.0, 2.5, -7.0] {
        assert_eq!(run(&p, x).to_bits(), run(&q, x).to_bits(), "f({x})");
    }
    (p, q)
}

#[test]
fn whole_elements_are_stored_as_integers() {
    let (p, q) = narrowed(Extra::None);
    assert_eq!(
        derefs(&q, Ty::F64),
        0,
        "every element access is an integer one"
    );
    assert_eq!(derefs(&q, Ty::I64), derefs(&p, Ty::F64));
    assert_eq!(run(&q, 0.5), 3.0 + 1.0 + 0.5);
}

#[test]
fn a_fractional_store_keeps_doubles() {
    let (_, q) = narrowed(Extra::Fraction);
    assert_eq!(derefs(&q, Ty::I64), 0);
}

#[test]
fn a_negative_zero_store_keeps_doubles() {
    let (_, q) = narrowed(Extra::NegZero);
    assert_eq!(derefs(&q, Ty::I64), 0);
}

#[test]
fn an_escaping_array_keeps_doubles() {
    let (_, q) = narrowed(Extra::Escapes);
    assert_eq!(derefs(&q, Ty::I64), 0);
}

#[test]
fn an_element_without_a_bound_keeps_doubles() {
    let (_, q) = narrowed(Extra::Grows);
    assert_eq!(derefs(&q, Ty::I64), 0);
}

#[test]
fn a_call_storing_into_an_element_keeps_doubles() {
    let (_, q) = narrowed(Extra::CallResult);
    assert_eq!(derefs(&q, Ty::I64), 0);
}
