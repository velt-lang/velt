//! `tests/golden/m2/classes.vlt` hand-lowered to HIR: class objects, constructors with field
//! defaults, `super(...)`, virtual dispatch through vtables, `Animal[]` holding a `Dog`, moves and
//! `clone()` of objects.

use velt_sema::hir::{
    AdtKind, BinOp as B, Callee, Def, Intrinsic, PassMode, Program, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::{m2_golden, run};

fn str_field(f: &FB, this: velt_sema::hir::LocalId, idx: u32, t: T) -> velt_sema::hir::Expr {
    field(f.bw(this), idx, U::Borrow, t.str)
}

pub(super) fn classes() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    // ---- Counter
    let counter_d = pb.declare();
    let counter = pb.adt_ty(counter_d, vec![]);
    let counter_ctor = pb.declare();
    let inc = pb.declare();
    let describe = pb.declare();
    let mut cd = adt(
        "Counter",
        AdtKind::Class,
        vec![("name", t.str, None), ("count", t.i64, Some(int(0, t.i64)))],
    );
    cd.ctor = Some(counter_ctor);
    pb.set_def(counter_d, Def::Adt(cd));
    {
        let mut f = FB::method("Counter.constructor", counter, t.unit);
        let this = f.param("this", counter, PassMode::BorrowMut);
        let name = f.param("name", t.str, PassMode::Owned);
        let body = vec![se(assign(
            field(f.bm(this), 0, U::BorrowMut, t.str),
            f.mv(name),
            t,
        ))];
        pb.define(counter_ctor, f.build(body));
    }
    {
        let mut f = FB::method("Counter.inc", counter, t.i64);
        let this = f.param("this", counter, PassMode::BorrowMut);
        let body = vec![
            se(cassign(
                B::Add,
                field(f.bm(this), 1, U::BorrowMut, t.i64),
                int(1, t.i64),
                t,
            )),
            ret(Some(field(f.bw(this), 1, U::Copy, t.i64))),
        ];
        pb.define(inc, f.build(body));
    }
    {
        let mut f = FB::method("Counter.describe", counter, t.str);
        let this = f.param("this", counter, PassMode::Borrow);
        let s = concat(
            concat(str_field(&f, this, 0, t), s("=", t), t),
            to_s(field(f.bw(this), 1, U::Copy, t.i64), t),
            t,
        );
        pb.define(describe, f.build(vec![ret(Some(s))]));
    }
    // ---- Animal / Dog
    let animal_d = pb.declare();
    let animal = pb.adt_ty(animal_d, vec![]);
    let dog_d = pb.declare();
    let dog = pb.adt_ty(dog_d, vec![]);
    let (animal_ctor, speak, intro, dog_ctor, dog_speak) = (
        pb.declare(),
        pb.declare(),
        pb.declare(),
        pb.declare(),
        pb.declare(),
    );
    let mut ad = adt("Animal", AdtKind::Class, vec![("name", t.str, None)]);
    ad.ctor = Some(animal_ctor);
    ad.vtable = vec![speak];
    pb.set_def(animal_d, Def::Adt(ad));
    let mut dd = adt(
        "Dog",
        AdtKind::Class,
        vec![
            ("name", t.str, None),
            ("tricks", t.i64, Some(int(2, t.i64))),
        ],
    );
    dd.ctor = Some(dog_ctor);
    dd.base = Some(animal);
    dd.vtable = vec![dog_speak];
    pb.set_def(dog_d, Def::Adt(dd));
    {
        let mut f = FB::method("Animal.constructor", animal, t.unit);
        let this = f.param("this", animal, PassMode::BorrowMut);
        let name = f.param("name", t.str, PassMode::Owned);
        let body = vec![se(assign(
            field(f.bm(this), 0, U::BorrowMut, t.str),
            f.mv(name),
            t,
        ))];
        pb.define(animal_ctor, f.build(body));
    }
    for (def, cls, name, suffix) in [
        (speak, animal, "Animal.speak", " makes a sound"),
        (dog_speak, dog, "Dog.speak", " barks"),
    ] {
        let mut f = FB::method(name, cls, t.str);
        let this = f.param("this", cls, PassMode::Borrow);
        let body = vec![ret(Some(concat(
            str_field(&f, this, 0, t),
            s(suffix, t),
            t,
        )))];
        pb.define(def, f.build(body));
    }
    {
        let mut f = FB::method("Animal.intro", animal, t.str);
        let this = f.param("this", animal, PassMode::Borrow);
        let sp = callee(Callee::Virtual { slot: 0 }, vec![f.bw(this)], t.str);
        let s1 = concat(
            concat(s("I am ", t), str_field(&f, this, 0, t), t),
            s(": ", t),
            t,
        );
        pb.define(intro, f.build(vec![ret(Some(concat(s1, sp, t)))]));
    }
    {
        let mut f = FB::method("Dog.constructor", dog, t.unit);
        let this = f.param("this", dog, PassMode::BorrowMut);
        let name = f.param("name", t.str, PassMode::Owned);
        let sup = call(
            animal_ctor,
            vec![upcast(f.bm(this), animal), f.mv(name)],
            t.unit,
        );
        pb.define(dog_ctor, f.build(vec![se(sup)]));
    }
    // ---- describeAll(xs: Animal[])
    let animals_t = pb.arr(animal);
    let describe_all = {
        let mut f = FB::new("describeAll", t.unit);
        let xs = f.param("xs", animals_t, PassMode::Borrow);
        let a = f.local("a", animal);
        let body = vec![for_of(
            pbind(a, U::Borrow, animal),
            f.bw(xs),
            vec![se(print(vec![call(intro, vec![f.bw(a)], t.str)], t))],
        )];
        pb.add_fn(f.build(body))
    };
    // ---- main
    let mut f = FB::new("main", t.unit);
    let c = f.local("c", counter);
    let d = f.local("d", dog);
    let animals = f.local("animals", animals_t);
    let moved = f.local("moved", counter);
    let copy = f.local("copy", counter);
    let clone = |e| intr(Intrinsic::Clone, vec![e], counter);
    let body = vec![
        let_(c, new_obj(counter_d, vec![s("clicks", t)], counter)),
        se(call(inc, vec![f.bm(c)], t.i64)),
        se(call(inc, vec![f.bm(c)], t.i64)),
        se(print(
            vec![
                call(inc, vec![f.bm(c)], t.i64),
                call(describe, vec![f.bw(c)], t.str),
            ],
            t,
        )),
        let_(d, new_obj(dog_d, vec![s("rex", t)], dog)),
        se(print(
            vec![
                call(dog_speak, vec![f.bw(d)], t.str),
                field(f.bw(d), 1, U::Copy, t.i64),
            ],
            t,
        )),
        let_(
            animals,
            array(
                vec![
                    new_obj(animal_d, vec![s("cat", t)], animal),
                    upcast(new_obj(dog_d, vec![s("fido", t)], dog), animal),
                ],
                animals_t,
            ),
        ),
        se(call(describe_all, vec![f.bw(animals)], t.unit)),
        let_(moved, f.mv(c)),
        se(print(vec![call(describe, vec![f.bw(moved)], t.str)], t)),
        let_(copy, clone(f.bw(moved))),
        se(print(
            vec![
                call(describe, vec![f.bw(copy)], t.str),
                call(describe, vec![f.bw(moved)], t.str),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_classes() {
    let out = run(&classes());
    assert_eq!(out.stdout, m2_golden("classes"));
    assert_eq!(out.code, 0);
}

#[test]
fn print_objects_and_arrays() {
    let mut pb = PB::new();
    let t = pb.t;
    let pd = pb.declare();
    let pt = pb.adt_ty(pd, vec![]);
    pb.set_def(
        pd,
        Def::Adt(adt(
            "Point",
            AdtKind::Struct,
            vec![("x", t.f64, None), ("name", t.str, None)],
        )),
    );
    let ad = pb.declare();
    let anon = pb.adt_ty(ad, vec![]);
    pb.set_def(
        ad,
        Def::Adt(adt("{anon}", AdtKind::Anon, vec![("n", t.i64, None)])),
    );
    let sa = pb.arr(t.str);
    let ia = pb.arr(t.i64);
    let iaa = pb.arr(ia);
    let os = pb.opt(t.str);
    let f = FB::new("main", t.unit);
    let body = vec![
        se(print(
            vec![array(vec![int(1, t.i64), int(2, t.i64), int(3, t.i64)], ia)],
            t,
        )),
        se(print(
            vec![array(vec![], ia), array(vec![s("a", t), s("b", t)], sa)],
            t,
        )),
        se(print(
            vec![array(
                vec![array(vec![int(1, t.i64)], ia), array(vec![], ia)],
                iaa,
            )],
            t,
        )),
        se(print(
            vec![
                adt_lit(pd, vec![flt(1.5, t.f64), s("p", t)], pt),
                adt_lit(ad, vec![int(7, t.i64)], anon),
            ],
            t,
        )),
        se(print(vec![null(os), wrap_some(s("x", t), os)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "[ 1, 2, 3 ]\n[] [ 'a', 'b' ]\n[ [ 1 ], [] ]\nPoint { x: 1.5, name: 'p' } { n: 7 }\nnull x\n"
    );
}
