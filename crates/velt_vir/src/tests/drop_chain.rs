//! Drop of self-referential classes (#543): the object drop glue of a list or tree class is a
//! loop that never calls itself (`lower/glue/drop_chain.rs`); one whose drop nests through an
//! array is bracketed for the runtime's depth limit (`lower/glue/drop_depth.rs`, emulated by
//! the interpreter with a small limit). Dropping chains frees every node (the interpreter fails
//! on leaks and double frees).

use velt_sema::hir::{AdtKind, BinOp as B, Def, Program, TyId, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::{lower_ok, run};
use crate::vir::{self, Callee, Terminator};

/// A class whose fields are `fields` (a field of type `None` is `Self | null`), and a `main`
/// that builds a chain of `n` objects linked through field `link`, prints `n` and drops it.
fn chain(fields: &[(&str, Option<TyId>)], link: u32, n: u128) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let d = pb.declare();
    let node = pb.adt_ty(d, vec![]);
    let opt = pb.opt(node);
    let defs = fields
        .iter()
        .map(|&(name, ty)| match ty {
            Some(ty) if ty == t.str => (name, ty, Some(s("x", t))),
            Some(ty) => (name, ty, Some(int(0, ty))),
            None => (name, opt, Some(null(opt))),
        })
        .collect();
    pb.set_def(d, Def::Adt(adt("Node", AdtKind::Class, defs)));
    let mut f = FB::new("main", t.unit);
    let head = f.local("head", opt);
    let i = f.local("i", t.i64);
    let cur = f.local("cur", node);
    let body = vec![
        let_(head, null(opt)),
        let_(i, int(0, t.i64)),
        while_(
            None,
            cmp(B::Lt, f.cp(i), int(n, t.i64), t),
            vec![
                let_(cur, new_obj(d, vec![], node)),
                se(assign(
                    field(f.bm(cur), link, U::BorrowMut, opt),
                    f.mv(head),
                    t,
                )),
                se(assign(f.bm(head), wrap_some(f.mv(cur), opt), t)),
                se(cassign(B::Add, f.bm(i), int(1, t.i64), t)),
            ],
            None,
        ),
        se(print(vec![f.cp(i)], t)),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

/// A class `Node { id, next: Node[] }` and a `main` that builds a chain of `n` nodes through
/// the arrays, prints `n` and drops it.
fn array_chain(n: u128) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let d = pb.declare();
    let node = pb.adt_ty(d, vec![]);
    let next = pb.arr(node);
    let fields = vec![
        ("id", t.i64, Some(int(0, t.i64))),
        ("next", next, Some(array(vec![], next))),
    ];
    pb.set_def(d, Def::Adt(adt("Node", AdtKind::Class, fields)));
    let mut f = FB::new("main", t.unit);
    let head = f.local("head", node);
    let i = f.local("i", t.i64);
    let cur = f.local("cur", node);
    let body = vec![
        let_(head, new_obj(d, vec![], node)),
        let_(i, int(1, t.i64)),
        while_(
            None,
            cmp(B::Lt, f.cp(i), int(n, t.i64), t),
            vec![
                let_(cur, new_obj(d, vec![], node)),
                se(assign(
                    field(f.bm(cur), 1, U::BorrowMut, next),
                    array(vec![f.mv(head)], next),
                    t,
                )),
                se(assign(f.bm(head), f.mv(cur), t)),
                se(cassign(B::Add, f.bm(i), int(1, t.i64), t)),
            ],
            None,
        ),
        se(print(vec![f.cp(i)], t)),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

/// The object drop glue of class `Node`, with its function index.
fn node_drop_glue(v: &vir::Program) -> (usize, &vir::Function) {
    v.funcs
        .iter()
        .enumerate()
        .find(|(_, f)| f.symbol.contains("objdrop"))
        .unwrap_or_else(|| panic!("no object drop glue for Node:\n{v}"))
}

/// Does the object drop glue of class `Node` call itself?
fn drop_glue_recurses(v: &vir::Program) -> bool {
    let (id, glue) = node_drop_glue(v);
    glue.blocks.iter().any(|b| {
        matches!(&b.term, Terminator::Call { callee: Callee::Func(f), .. } if f.0 as usize == id)
    })
}

/// Is the object drop glue of class `Node` bracketed for the runtime's depth limit?
fn drop_glue_bracketed(v: &vir::Program) -> bool {
    let (_, glue) = node_drop_glue(v);
    glue.blocks.iter().any(|b| {
        matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. }
            if v.externs[e.0 as usize].symbol == "velt_rt_drop_enter")
    })
}

#[test]
fn a_linked_list_drops_in_a_loop() {
    let t = PB::new().t;
    let p = chain(&[("value", Some(t.i64)), ("next", None)], 1, 200);
    let v = lower_ok(&p);
    assert!(!drop_glue_recurses(&v), "the list drop recurses:\n{v}");
    assert!(
        !drop_glue_bracketed(&v),
        "a plain list needs no bracket:\n{v}"
    );
    assert_eq!(run(&p).stdout, "200\n");
}

#[test]
fn a_degenerate_tree_drops_in_a_loop() {
    // The chain runs through `left`, which is not the tail: the loop rotates it onto `right`.
    let t = PB::new().t;
    let fields = [("label", Some(t.str)), ("left", None), ("right", None)];
    let p = chain(&fields, 1, 200);
    let v = lower_ok(&p);
    assert!(!drop_glue_recurses(&v), "the tree drop recurses:\n{v}");
    assert_eq!(run(&p).stdout, "200\n");
    // Through the tail as well.
    assert_eq!(run(&chain(&fields, 2, 200)).stdout, "200\n");
}

#[test]
fn a_silent_field_after_the_link_still_loops() {
    // A string after the link drops silently, so the loop may run it first.
    let t = PB::new().t;
    let p = chain(&[("next", None), ("name", Some(t.str))], 0, 50);
    assert!(!drop_glue_recurses(&lower_ok(&p)));
    assert_eq!(run(&p).stdout, "50\n");
}

#[test]
fn a_chain_through_arrays_is_bracketed_and_drops_every_node() {
    // The interpreter queues every drop past depth 4: the chain is dropped from the queue.
    let p = array_chain(200);
    let v = lower_ok(&p);
    assert!(
        drop_glue_bracketed(&v),
        "the array chain drop is not bracketed:
{v}"
    );
    assert_eq!(
        run(&p).stdout,
        "200
"
    );
}
