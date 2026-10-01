//! Consuming `for…of` (`ForOf { consume: true }`): elements are moved into the binding; the
//! elements not reached are dropped on `break`, `return` and `throw`, and the buffer is freed
//! exactly once (the interpreter fails on leaks and double frees).

use velt_sema::hir::{
    BinOp as B, DefId, Expr, Intrinsic as I, LocalId, Pat, Program, Stmt, StmtKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::run;

fn consume_loop(binding: Pat, iter: Expr, body: Vec<Stmt>) -> Stmt {
    st(StmtKind::ForOf {
        label: None,
        binding,
        iter,
        body: block(body),
        consume: true,
    })
}

/// `make(): string[]` returning four heap strings `a1 b2 c3 d4`.
fn make(pb: &mut PB) -> DefId {
    let t = pb.t;
    let sa = pb.arr(t.str);
    let f = FB::new("make", sa);
    let elems = ["a", "b", "c", "d"]
        .iter()
        .enumerate()
        .map(|(i, p)| concat(s(p, t), s(&(i + 1).to_string(), t), t))
        .collect();
    pb.add_fn(f.build(vec![ret(Some(array(elems, sa)))]))
}

/// `pick(): string` returns the element after `b2` from inside the loop.
fn pick(pb: &mut PB, mk: DefId) -> DefId {
    let t = pb.t;
    let sa = pb.arr(t.str);
    let mut f = FB::new("pick", t.str);
    let seen = f.local("seen", t.bool);
    let x = f.local("x", t.str);
    let is_b2 = cmp(B::Eq, f.bw(x), s("b2", t), t);
    let body = vec![
        let_(seen, boolean(false, t)),
        consume_loop(
            pbind(x, U::Move, t.str),
            call(mk, vec![], sa),
            vec![
                if_(f.cp(seen), vec![ret(Some(f.mv(x)))], None),
                se(assign(f.bm(seen), is_b2, t)),
            ],
        ),
        ret(Some(s("none", t))),
    ];
    pb.add_fn(f.build(body))
}

/// `fail(): void throws string` throws the second element.
fn fail(pb: &mut PB, mk: DefId) -> DefId {
    let t = pb.t;
    let sa = pb.arr(t.str);
    let mut f = FB::new("fail", t.unit);
    f.throws = Some(t.str);
    let n = f.local("n", t.i64);
    let x = f.local("x", t.str);
    let body = vec![
        let_(n, int(0, t.i64)),
        consume_loop(
            pbind(x, U::Move, t.str),
            call(mk, vec![], sa),
            vec![
                se(cassign(B::Add, f.bm(n), int(1, t.i64), t)),
                if_(
                    cmp(B::Eq, f.cp(n), int(2, t.i64), t),
                    vec![se(throw(f.mv(x), t.never))],
                    None,
                ),
            ],
        ),
    ];
    pb.add_fn(f.build(body))
}

fn program() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let sa = pb.arr(t.str);
    let mk = make(&mut pb);
    let pick = pick(&mut pb, mk);
    let fail = fail(&mut pb, mk);
    let mut f = FB::new("main", t.unit);
    let out = f.local("out", sa);
    let x = f.local("x", t.str);
    let y = f.local("y", t.str);
    let e = f.local("e", t.str);
    let len = |f: &FB, l: LocalId| intr(I::ArrayLen, vec![f.bw(l)], t.usize);
    let push = intr(I::ArrayPush, vec![f.bm(out), f.mv(x)], t.unit);
    let body = vec![
        let_(out, array(vec![], sa)),
        // `break` after moving two elements: the third is dropped with the binding, the
        // fourth by the loop.
        consume_loop(
            pbind(x, U::Move, t.str),
            call(mk, vec![], sa),
            vec![
                if_(
                    cmp(B::Eq, len(&f, out), int(2, t.usize), t),
                    vec![brk(None)],
                    None,
                ),
                se(push),
            ],
        ),
        se(print(vec![f.bw(out)], t)),
        // Never moved: every element is dropped at the end of its iteration.
        consume_loop(
            pbind(y, U::Move, t.str),
            call(mk, vec![], sa),
            vec![cont(None)],
        ),
        se(print(vec![call(pick, vec![], t.str)], t)),
        try_(
            vec![se(call(fail, vec![], t.unit))],
            Some((Some(e), vec![se(print(vec![s("caught", t), f.bw(e)], t))])),
            None,
        ),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn consuming_loop_drops_what_it_did_not_move() {
    let out = run(&program());
    assert_eq!(out.stdout, "[ 'a1', 'b2' ]\nc3\ncaught b2\n");
}
