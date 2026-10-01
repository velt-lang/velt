//! Builtin `Comparable<T>` (std/prelude/compare.vlt): ordering operators on bounded params,
//! impls from `extend` blocks, and extension lookup preferring exact targets.

mod common;

use common::hir_walk::{calls, exprs, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{BinOp, Callee, Def, ExprKind as E, TyKind};

#[test]
fn ordering_on_a_comparable_param_calls_compare_to() {
    let p = ok_src(
        "function less<T extends Comparable<T>>(a: T, b: T): bool { return a < b; }
         function main() { console.log(less(1, 2), less(\"b\", \"a\")); }",
    );
    let f = func(&p, "less");
    // `compareTo(a, b) < 0`
    let found = exprs(f).into_iter().any(|e| match &e.kind {
        E::Binary {
            op: BinOp::Lt, lhs, ..
        } => {
            matches!(&lhs.kind, E::Call { callee: Callee::ParamMethod { iface, slot: 0, .. }, args }
            if args.len() == 2
                && matches!(p.def(*iface), Def::Interface(i) if i.name.ends_with("Comparable")))
        }
        _ => false,
    });
    assert!(found, "`a < b` is not `a.compareTo(b) < 0` in `less`");
}

#[test]
fn builtin_types_are_comparable_through_extend_impls() {
    let p = ok_src(
        "function lt<T extends Comparable<T>>(a: T, b: T): bool { return a < b; }
         function main() { console.log(lt(1u8, 2u8), lt(1.5f32, 0.5f32), lt(false, true)); }",
    );
    let comparable_tys: Vec<&TyKind> = p
        .impls
        .iter()
        .filter(|i| matches!(p.def(i.iface), Def::Interface(d) if d.name.ends_with("Comparable")))
        .map(|i| p.types.kind(i.ty))
        .collect();
    for want in [TyKind::Bool, TyKind::Str] {
        assert!(comparable_tys.contains(&&want), "{want:?} not Comparable");
    }
    assert!(comparable_tys.len() >= 14, "{comparable_tys:?}");
}

#[test]
fn unbounded_params_and_non_comparable_types_are_rejected() {
    let r = err_src("function lt<T>(a: T, b: T): bool { return a < b; } function main() {}");
    assert!(
        r.contains("cannot apply binary operator `<` to type `T`"),
        "{r}"
    );
    let r = err_src(
        "struct P { x: i64; }
         function lt<T extends Comparable<T>>(a: T, b: T): bool { return a < b; }
         function main() { console.log(lt(P { x: 1 }, P { x: 2 })); }",
    );
    assert!(r.contains("does not implement `Comparable`"), "{r}");
}

#[test]
fn user_types_implement_comparable() {
    ok_src(
        "class V implements Comparable<V> { n: i64 = 0; compareTo(o: V): i64 { return this.n - o.n; } }
         struct P { x: i64; }
         extend P { compareTo(o: P): i64 { return this.x.compareTo(o.x); } }
         function lt<T extends Comparable<T>>(a: T, b: T): bool { return a < b && !(a >= b); }
         function main() { console.log(lt(new V(), new V()), lt(P { x: 1 }, P { x: 2 })); }",
    );
}

#[test]
fn exact_extensions_win_and_generic_ones_need_their_bounds() {
    let p = ok_src(
        "struct P { x: i64; }
         extend<T extends Comparable<T>> Array<T> { best(): i64 { return 1; } }
         extend Array<i64> { best(): i64 { return 2; } }
         extend<T> Array<T> { best(): i64 { return 3; } }
         function main() {
           const a: i64[] = [1]; const b: string[] = [\"x\"]; const c: P[] = [];
           console.log(a.best(), b.best(), c.best());
         }",
    );
    let main = func(&p, "main");
    let targets: Vec<String> = calls(main)
        .into_iter()
        .filter_map(|(c, _)| match c {
            Callee::Def(d, _) => match p.def(*d) {
                Def::Fn(f) if f.name.contains(".best") => Some(f.name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(targets.len(), 3, "{targets:?}");
    assert!(targets[0].contains("i64"), "{targets:?}");
    assert_ne!(targets[1], targets[0], "{targets:?}");
    assert_ne!(targets[2], targets[1], "{targets:?}");
}
