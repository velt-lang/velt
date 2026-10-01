//! HIR shape of the M2 golden programs: the encodings the IR relies on (dispatch, upcasts,
//! interface values, captures, ownership modes, throws, narrowing).

mod common;

use common::hir_walk::{adt, calls, def_id, exprs, func, pats, uses_of};
use common::programs::{load_file, repo_root};
use velt_sema::hir::{
    self, Callee, Def, ExprKind as E, Intrinsic, PassMode, PatKind, Program, TyKind, UseMode,
};

fn golden(name: &str) -> Program {
    let l = load_file(&repo_root().join(format!("tests/golden/m2/{name}.vlt")));
    let (p, d) = l.check();
    p.unwrap_or_else(|| panic!("{}", l.render(&d)))
}

fn has_callee(f: &hir::FnDef, pred: impl Fn(&Callee) -> bool) -> bool {
    calls(f).iter().any(|(c, _)| pred(c))
}

#[test]
fn classes_vtables_and_dispatch() {
    let p = golden("classes");
    let animal = adt(&p, "Animal");
    let dog = adt(&p, "Dog");
    assert_eq!(animal.vtable, vec![def_id(&p, "Animal.speak")]);
    assert_eq!(dog.vtable, vec![def_id(&p, "Dog.speak")]);
    assert_eq!(
        dog.fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["name", "tricks"]
    );
    assert!(dog.base.is_some());
    assert!(dog.fields[1].default.is_some());
    assert_eq!(adt(&p, "Counter").vtable, vec![]);
    // `this.speak()` in a base-class method dispatches through the vtable.
    assert!(has_callee(func(&p, "Animal.intro"), |c| matches!(
        c,
        Callee::Virtual { slot: 0 }
    )));
    // `c.inc()` is a direct call with a mutably borrowed receiver.
    let main = func(&p, "main");
    let inc = def_id(&p, "Counter.inc");
    let inc_call = calls(main)
        .into_iter()
        .find(|(c, _)| matches!(c, Callee::Def(d, _) if *d == inc))
        .expect("inc call");
    assert!(matches!(
        inc_call.1[0].kind,
        E::Local(_, UseMode::BorrowMut)
    ));
    // A Dog in an Animal[] literal is upcast.
    assert!(exprs(main)
        .iter()
        .any(|e| matches!(&e.kind, E::ArrayLit(xs) if matches!(xs[1].kind, E::Upcast(_)))));
    // `const moved = c;` moves.
    assert!(uses_of(main, "c").contains(&UseMode::Move));
}

#[test]
fn constructors_own_their_params() {
    let p = golden("classes");
    let ctor = func(&p, "Counter.constructor");
    assert_eq!(ctor.params[0].mode, PassMode::BorrowMut);
    assert_eq!(ctor.params[1].mode, PassMode::Owned);
    let dog_ctor = func(&p, "Dog.constructor");
    assert_eq!(
        dog_ctor.params[1].mode,
        PassMode::Owned,
        "moved on into super(...)"
    );
    let base_ctor = def_id(&p, "Animal.constructor");
    let (_, args) = calls(dog_ctor)
        .into_iter()
        .find(|(c, _)| matches!(c, Callee::Def(d, _) if *d == base_ctor))
        .expect("super call");
    assert!(matches!(args[0].kind, E::Upcast(_)));
    assert!(matches!(args[1].kind, E::Local(_, UseMode::Move)));
    assert_eq!(adt(&p, "Dog").ctor, Some(def_id(&p, "Dog.constructor")));
}

#[test]
fn generics_inference_bounds_and_interfaces() {
    let p = golden("generics");
    let swap = func(&p, "swap");
    assert_eq!(swap.generics, 2);
    assert_eq!(swap.params[0].mode, PassMode::Owned);
    assert!(exprs(swap).iter().any(|e| matches!(
        e.kind,
        E::Field {
            mode: UseMode::Move,
            ..
        }
    )));
    let main = func(&p, "main");
    assert!(uses_of(main, "p").contains(&UseMode::Move));
    // `x.area()` on a bounded `T` and `this.area()` in a default method.
    assert!(has_callee(func(&p, "biggest"), |c| matches!(
        c,
        Callee::ParamMethod { slot: 0, .. }
    )));
    assert!(has_callee(func(&p, "Shape.describe"), |c| matches!(
        c,
        Callee::ParamMethod { slot: 0, .. }
    )));
    // `sh.describe()` on an interface value.
    assert!(has_callee(main, |c| matches!(c, Callee::Dyn { slot: 1 })));
    let to_dyn: Vec<u32> = exprs(main)
        .iter()
        .filter_map(|e| match e.kind {
            E::ToDyn { impl_index, .. } => Some(impl_index),
            _ => None,
        })
        .collect();
    assert_eq!(to_dyn.len(), 2);
    let square = def_id(&p, "Square");
    let circle = def_id(&p, "Circle");
    let ty_def = |i: u32| match p.types.kind(p.impls[i as usize].ty) {
        TyKind::Adt(d, _) => *d,
        _ => panic!("impl type"),
    };
    assert_eq!(ty_def(to_dyn[0]), square);
    assert_eq!(ty_def(to_dyn[1]), circle);
    let describe = def_id(&p, "Shape.describe");
    let sq_impl = p
        .impls
        .iter()
        .find(|i| matches!(p.types.kind(i.ty), TyKind::Adt(d, _) if *d == square))
        .unwrap();
    assert_eq!(sq_impl.methods, vec![def_id(&p, "Square.area"), describe]);
    assert_eq!(func(&p, "Stack.push").params[1].mode, PassMode::Owned);
    assert!(has_callee(func(&p, "mapAll"), |c| matches!(
        c,
        Callee::Indirect(_)
    )));
    // `mapAll([1, 2, 3], (x) => x * 2)` instantiates T = U = i64.
    let map_all = def_id(&p, "mapAll");
    let i64_ = p.types.get(&TyKind::Int(hir::IntTy::I64)).unwrap();
    assert!(calls(main)
        .iter()
        .any(|(c, _)| matches!(c, Callee::Def(d, ts) if *d == map_all && ts == &vec![i64_, i64_])));
}

#[test]
fn closures_capture_modes() {
    let p = golden("closures");
    let closure_of = |fn_name: &str| {
        p.defs
            .iter()
            .filter_map(|d| match d {
                Def::Fn(f) if f.name.starts_with(&format!("{fn_name}::{{closure#")) => Some(f),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let counter = closure_of("makeCounter");
    assert_eq!(counter[0].captures.len(), 1);
    assert_eq!(counter[0].captures[0].mode, PassMode::Owned);
    assert_eq!(counter[0].params[0].local, counter[0].captures[0].inner);
    assert_eq!(closure_of("makeAdder")[0].captures[0].mode, PassMode::Copy);
    let main_closures = closure_of("main");
    let modes: Vec<Vec<PassMode>> = main_closures
        .iter()
        .map(|c| c.captures.iter().map(|x| x.mode).collect())
        .collect();
    // forEach(total += x), apply(x * factor), greet (moves `name`), map/filter/reduce (none).
    assert!(modes.contains(&vec![PassMode::BorrowMut]));
    assert!(modes.contains(&vec![PassMode::Copy]));
    assert!(modes.contains(&vec![PassMode::Owned]));
    let main = func(&p, "main");
    assert!(has_callee(main, |c| matches!(c, Callee::Indirect(_))));
    for c in main_closures {
        for (i, cap) in c.captures.iter().enumerate() {
            assert_eq!(cap.inner.0 as usize, i, "captures are the leading locals");
        }
    }
}

#[test]
fn throws_and_union_values() {
    let p = golden("errors");
    let perr = def_id(&p, "ParseError");
    let is_perr = |t: Option<hir::TyId>| matches!(t.map(|t| p.types.kind(t)), Some(TyKind::Adt(d, _)) if *d == perr);
    assert!(is_perr(func(&p, "parseDigit").throws));
    assert!(is_perr(func(&p, "sumDigits").throws));
    assert!(
        is_perr(func(&p, "main").throws),
        "an uncaught call outside try in main"
    );
    assert!(func(&p, "safeDiv").throws.is_none());
    let main = func(&p, "main");
    assert!(exprs(func(&p, "useValues"))
        .iter()
        .any(|e| matches!(e.kind, E::UnwrapVariant { .. })));
    assert!(
        exprs(main)
            .iter()
            .any(|e| matches!(e.kind, E::UnwrapSome(..))),
        "narrowed `idx`"
    );
    assert!(exprs(func(&p, "find"))
        .iter()
        .any(|e| matches!(e.kind, E::WrapSome(_))));
    let has_try = main.body.block.stmts.iter().any(|s| matches!(&s.kind, hir::StmtKind::Try { catch: Some((Some(l), _)), .. } if is_perr(Some(main.body.locals[l.0 as usize].ty))));
    assert!(has_try, "catch variable has the thrown class type");
}

#[test]
fn enums_and_discriminated_unions() {
    let p = golden("enums_switch");
    let color = match p.def(def_id(&p, "Color")) {
        Def::Enum(e) => e,
        _ => panic!(),
    };
    assert_eq!(
        color
            .variants
            .iter()
            .map(|v| v.discriminant)
            .collect::<Vec<_>>(),
        [0, 5, 6]
    );
    // `Shape` is a union of anonymous object types: a compiler-generated union enum with one
    // variant per member (the discriminant is zero-sized: the tag is the variant).
    let shape = p
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Enum(e) if e.is_union && e.name.contains("\"circle\"") => Some(e),
            _ => None,
        })
        .expect("the Shape union");
    // Object types are values with one owner, like classes (only `struct`s are Copy), so a
    // callback that changes one changes the stored object.
    assert!(!shape.is_copy);
    assert_eq!(shape.variants.len(), 3);
    // `switch (s.kind)` matches the variants of `s` directly.
    let variant_pats = pats(func(&p, "area"))
        .iter()
        .filter(|x| matches!(x.kind, PatKind::Variant { .. }))
        .count();
    assert_eq!(variant_pats, 3);
    // `case 0: case 1:` share one arm.
    assert!(pats(func(&p, "grade"))
        .iter()
        .any(|x| matches!(x.kind, PatKind::Or(_))));
}

#[test]
fn arrays_maps_and_destructuring() {
    let p = golden("arrays_maps");
    let main = func(&p, "main");
    assert!(has_callee(main, |c| matches!(
        c,
        Callee::Intrinsic(Intrinsic::ArrayPush)
    )));
    assert!(has_callee(main, |c| matches!(
        c,
        Callee::Intrinsic(Intrinsic::ArrayPop)
    )));
    assert!(exprs(main).iter().any(|e| matches!(&e.kind, E::Assign { place, .. } if matches!(place.kind, E::Index { mode: UseMode::BorrowMut, .. }))));
    let let_pats: Vec<&hir::Pat> = main
        .body
        .block
        .stmts
        .iter()
        .filter_map(|s| match &s.kind {
            hir::StmtKind::LetPat { pat, .. } => Some(pat),
            _ => None,
        })
        .collect();
    assert!(matches!(let_pats[0].kind, PatKind::Array { .. }));
    assert!(matches!(let_pats[1].kind, PatKind::Adt { .. }));
    assert!(p
        .defs
        .iter()
        .any(|d| matches!(d, Def::Adt(a) if a.name == "std/prelude/map::Map")));
}

#[test]
fn structs_are_objects() {
    let p = golden("structs");
    // Semantics stage 2: structs are objects (references), never Copy.
    assert!(!adt(&p, "Point").is_copy);
    let main = func(&p, "main");
    assert!(
        uses_of(main, "p").contains(&UseMode::BorrowMut),
        "p.scale(2.0)"
    );
    assert!(p
        .defs
        .iter()
        .any(|d| matches!(d, Def::Adt(a) if a.kind == hir::AdtKind::Anon && !a.is_copy)));
}

#[test]
fn modules_and_constants() {
    let p = golden("modules");
    assert!(p
        .defs
        .iter()
        .any(|d| matches!(d, Def::Global(g) if g.name == "_geometry::ORIGIN")));
    assert!(exprs(func(&p, "main"))
        .iter()
        .any(|e| matches!(e.kind, E::Global(_))));
    assert_eq!(
        func(&p, "_geometry::Registry.add").params[1].mode,
        PassMode::Owned
    );
}
