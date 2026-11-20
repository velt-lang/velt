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
