//! `isUnique(x)` (hir `Intrinsic::IsUnique`, lower/rc.rs `is_unique`): its answer for each kind of
//! value, run in the interpreter. A wrong `true` would let copy-on-write code change a value
//! another reference sees, so every kind that may be shared answers `false` while it is, and a
//! counted object's answer reads its whole count word (`main` compares it with 1, unmasked: a
//! flag in the word, such as the cycle collector's `BUFFERED`, makes it not unique).

use super::{lower_ok, std_src};
use crate::vir::{self, BinOp, Operand, Rvalue, Stmt, Terminator};

/// `src`'s `main` (a std module, so it may import the intrinsics), lowered and run: its output,
/// and the program.
fn run(src: &str) -> (String, vir::Program) {
    let p = std_src::check_std_src(
        "is_unique_test",
        &format!("import {{ isUnique }} from \"velt/lang/intrinsics\";\n{src}"),
    );
    let v = lower_ok(&p);
    let out = super::interp::run(&v);
    assert_eq!(out.live_allocs, 0, "leaked heap allocations\n{v}");
    (out.stdout, v)
}

#[test]
fn a_counted_object_is_unique_only_while_one_reference_holds_it() {
    let (out, v) = run("class C { n: i64 = 0; }
         function main() {
           const a = new C();
           console.log(isUnique(a));
           const xs: C[] = [a];
           console.log(isUnique(a), isUnique(xs[0]));
           xs.pop();
           console.log(isUnique(a));
           const b = a;
           console.log(isUnique(a), isUnique(b));
         }");
    assert_eq!(out, "true\nfalse false\ntrue\nfalse false\n");
    // The whole count word is compared with 1: no mask takes a flag out of it first.
    let main = (v.funcs.iter())
        .find(|f| f.symbol == "_V4main")
        .unwrap_or_else(|| panic!("no main in:\n{v}"));
    let stmts = || main.blocks.iter().flat_map(|b| &b.stmts);
    let eq_one = stmts().any(|s| {
        matches!(
            s,
            Stmt::Assign(
                _,
                Rvalue::Binary(
                    BinOp::Eq,
                    Operand::Copy(_),
                    Operand::Const(vir::Const::Int(1), vir::Ty::U64)
                )
            )
        )
    });
    assert!(eq_one, "no count word compared with 1:\n{v}");
    let masked = stmts().any(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::BitAnd, ..))));
    assert!(!masked, "the count word is masked before the compare:\n{v}");
}

#[test]
fn a_counted_array_is_unique_only_while_one_reference_holds_it() {
    let (out, _) = run("function main() {
           const xs: i64[] = [1, 2];
           console.log(isUnique(xs));
           const ys = xs;
           console.log(isUnique(xs), isUnique(ys));
         }");
    assert_eq!(out, "true\nfalse false\n");
}

#[test]
fn a_type_the_program_never_counts_is_always_unique() {
    // `D` is never shared, so it has no count: each value has its one owner.
    let (out, _) = run("class D { n: i64 = 0; }
         function main() {
           const d = new D();
           d.n = 1;
           console.log(isUnique(d), d.n);
         }");
    assert_eq!(out, "true 1\n");
}

#[test]
fn shared_values_strings_and_functions_are_never_unique() {
    let (out, _) = run("class C { n: i64 = 0; }
         function main() {
           const s = shared(new C());
           const t = `x${s.n}`;
           const f = (x: i64): i64 => x + 1;
           console.log(isUnique(s), isUnique(t), isUnique(f), isUnique(1));
         }");
    assert_eq!(out, "false false false false\n");
}

#[test]
fn an_object_of_a_lock_set_type_is_never_unique() {
    // Readers of an `RwLock` share the counts of what it holds (boxing/locks.rs): such a type is
    // never taken as unique, even with one reference: `probe` answers `false` without reading a
    // count. (`L` is counted: `[k, k]` shares it. The interpreter has no `RwLock`: the lowering is
    // read.)
    let p = std_src::check_std_src(
        "is_unique_test",
        "import { isUnique } from \"velt/lang/intrinsics\";
         class L { n: i64 = 0; }
         function probe(k: L): bool { return isUnique(k); }
         function main() {
           const lock = shared(new RwLock(new L()));
           console.log(lock.read((l) => l.n));
           const k = new L();
           const ks: L[] = [k, k];
           console.log(probe(ks[0]));
         }",
    );
    let v = lower_ok(&p);
    let probe = (v.funcs.iter())
        .find(|f| f.symbol == "_V5probe")
        .unwrap_or_else(|| panic!("no probe in:\n{v}"));
    assert!(
        matches!(
            &probe.blocks[..],
            [b] if b.stmts.is_empty()
                && matches!(b.term, Terminator::Return(Operand::Const(vir::Const::Bool(false), _)))
        ),
        "a lock-set object is asked for its count:\n{v}"
    );
}

#[test]
fn an_optional_field_read_with_bang_is_read_in_place() {
    // `isUnique(this.t!)` reads the field's payload in place: a share of it (sema's `!` is a
    // `match` binding the payload) would count it twice, and a null is not unique.
    let (out, v) = run("class N { n: i64 = 0; }
         class H {
           t: N | null = null;
           opt(): bool { return isUnique(this.t!); }
         }
         function main() {
           const h = new H();
           console.log(h.opt());
           h.t = new N();
           console.log(h.opt());
           const keep = h.t!;
           console.log(h.opt(), keep.n);
         }");
    assert_eq!(out, "false\ntrue\nfalse 0\n");
    let opt = (v.funcs.iter())
        .find(|f| f.symbol == "_V1HM3opt")
        .unwrap_or_else(|| panic!("no opt in:\n{v}"));
    let shares = (opt.blocks.iter())
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Add, ..))));
    assert!(
        !shares,
        "`opt` counts the field before reading its count:\n{v}"
    );
}

#[test]
fn a_weak_target_is_never_unique() {
    // A `WeakRef` sets a flag in its target's count word (`RC_WEAK`): the whole word is compared,
    // so the target is not unique even with one strong reference. The same class without one is.
    let (out, _) = run("class C { n: i64 = 0; }
         function main() {
           const a = new C();
           const b = new C();
           const w = new WeakRef(b);
           console.log(isUnique(a), isUnique(b), w.deref() != undefined);
         }");
    assert_eq!(out, "true false true\n");
}

#[test]
fn a_borrow_answers_for_the_reference_it_borrows() {
    // A borrowed parameter and a captured variable reach the object through the one reference
    // the caller's variable holds: `true` there means a change in place shows in that variable,
    // as in JavaScript. A second reference makes it `false`.
    let (out, _) = run("class C { n: i64 = 0; }
         function param(p: C): bool { return isUnique(p); }
         function main() {
           const a = new C();
           console.log(param(a));
           const two: C[] = [a];
           console.log(param(a));
           two.pop();
           const c = new C();
           const f = (): bool => isUnique(c);
           console.log(f(), c.n);
         }");
    assert_eq!(out, "true\nfalse\ntrue 0\n");
}
