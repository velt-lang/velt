//! Drop of self-referential classes as a loop (#543, `lower/glue/drop_chain.rs`): the object
//! drop glue of a list or tree class never calls itself, and dropping chains through it frees
//! every node (the interpreter fails on leaks and double frees).

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

/// The object drop glue of class `Node`, and whether it calls itself.
fn drop_glue_recurses(v: &vir::Program) -> bool {
    let (id, glue) = v
        .funcs
        .iter()
        .enumerate()
        .find(|(_, f)| f.symbol.contains("objdrop"))
        .unwrap_or_else(|| panic!("no object drop glue for Node:\n{v}"));
    glue.blocks.iter().any(|b| {
        matches!(&b.term, Terminator::Call { callee: Callee::Func(f), .. } if f.0 as usize == id)
    })
}

#[test]
fn a_linked_list_drops_in_a_loop() {
    let t = PB::new().t;
    let p = chain(&[("value", Some(t.i64)), ("next", None)], 1, 200);
    let v = lower_ok(&p);
    assert!(!drop_glue_recurses(&v), "the list drop recurses:\n{v}");
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
