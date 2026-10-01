//! Test-only prelude: the generic array helpers (`Array.forEach/map/filter/reduce`) written as
//! generic HIR functions, as `std/prelude` defines them.

use velt_sema::hir::{DefId, Intrinsic, PassMode, UseMode as U};

use super::builder::*;
use super::builder_m2::*;

/// Generic array helpers as the prelude defines them.
pub(super) struct Prelude {
    pub for_each: DefId,
    pub map: DefId,
    pub filter: DefId,
    pub reduce: DefId,
}

pub(super) fn prelude(pb: &mut PB) -> Prelude {
    let t = pb.t;
    let (p0, p1) = (pb.param(0), pb.param(1));
    let (a0, a1) = (pb.arr(p0), pb.arr(p1));
    let for_each = {
        let fty = pb.fn_ty(vec![p0], t.unit);
        let mut f = FB::new("Array.forEach", t.unit);
        f.generics = 1;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let cb = f.param("f", fty, PassMode::Borrow);
        let x = f.local("x", p0);
        let body = vec![for_of(
            pbind(x, U::Borrow, p0),
            f.bw(xs),
            vec![se(call_ptr(f.bw(cb), vec![f.bw(x)], t.unit))],
        )];
        pb.add_fn(f.build(body))
    };
    let map = {
        let fty = pb.fn_ty(vec![p0], p1);
        let mut f = FB::new("Array.map", a1);
        f.generics = 2;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let cb = f.param("f", fty, PassMode::Borrow);
        let out = f.local("out", a1);
        let x = f.local("x", p0);
        let push = intr(
            Intrinsic::ArrayPush,
            vec![f.bm(out), call_ptr(f.bw(cb), vec![f.bw(x)], p1)],
            t.unit,
        );
        let body = vec![
            let_(out, array(vec![], a1)),
            for_of(pbind(x, U::Borrow, p0), f.bw(xs), vec![se(push)]),
            ret(Some(f.mv(out))),
        ];
        pb.add_fn(f.build(body))
    };
    let filter = {
        let fty = pb.fn_ty(vec![p0], t.bool);
        let mut f = FB::new("Array.filter", a0);
        f.generics = 1;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let cb = f.param("f", fty, PassMode::Borrow);
        let out = f.local("out", a0);
        let x = f.local("x", p0);
        let push = intr(
            Intrinsic::ArrayPush,
            vec![f.bm(out), intr(Intrinsic::Clone, vec![f.bw(x)], p0)],
            t.unit,
        );
        let body = vec![
            let_(out, array(vec![], a0)),
            for_of(
                pbind(x, U::Borrow, p0),
                f.bw(xs),
                vec![if_(
                    call_ptr(f.bw(cb), vec![f.bw(x)], t.bool),
                    vec![se(push)],
                    None,
                )],
            ),
            ret(Some(f.mv(out))),
        ];
        pb.add_fn(f.build(body))
    };
    let reduce = {
        let fty = pb.fn_ty(vec![p1, p0], p1);
        let mut f = FB::new("Array.reduce", p1);
        f.generics = 2;
        let xs = f.param("xs", a0, PassMode::Borrow);
        let cb = f.param("f", fty, PassMode::Borrow);
        let init = f.param("init", p1, PassMode::Owned);
        let acc = f.local("acc", p1);
        let x = f.local("x", p0);
        let body = vec![
            let_(acc, f.mv(init)),
            for_of(
                pbind(x, U::Borrow, p0),
                f.bw(xs),
                vec![se(assign(
                    f.bm(acc),
                    call_ptr(f.bw(cb), vec![f.mv(acc), f.bw(x)], p1),
                    t,
                ))],
            ),
            ret(Some(f.mv(acc))),
        ];
        pb.add_fn(f.build(body))
    };
    Prelude {
        for_each,
        map,
        filter,
        reduce,
    }
}
