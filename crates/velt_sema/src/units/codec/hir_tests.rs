//! The HIR of a real program, std's prelude included, comes back from its encoding as it was.

use crate::script_locals::sema_tests::programs;

use std::time::Instant;

use super::{Codec, Reader, Writer};
use crate::hir::{Program, TyId, TyKind};

/// Classes with a base, an interface and its implementations, a generic function, enums and a
/// union, closures that capture, async code, `try`, a pattern, module state and a large number
/// table: most HIR shapes, besides the prelude's.
pub(super) const SRC: &str = r#"
interface Shape { area(): number; name(): string }
class Circle implements Shape {
    constructor(public r: number) {}
    area(): number { return Math.PI * this.r * this.r; }
    name(): string { return "circle"; }
}
class Square implements Shape {
    side = 2;
    area(): number { return this.side * this.side; }
    name(): string { return `square ${this.side}`; }
}
class Base { greet(): string { return "base"; } }
class Derived extends Base { override greet(): string { return "derived " + super.greet(); } }
enum Color { Red, Green, Blue = 7 }
enum Dir { Up = "UP", Down = "DOWN" }
type Kind = "a" | "b" | number;
const TABLE = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23];
let counter = 0;
function largest<T>(xs: T[], gt: (a: T, b: T) => boolean): T | undefined {
    let best: T | undefined = undefined;
    for (const x of xs) {
        if (best === undefined || gt(x, best)) best = x;
    }
    return best;
}
function kindOf(k: Kind): string {
    switch (k) {
        case "a": return "A";
        case "b": return "B";
        default: return `n${k}`;
    }
}
async function later(n: number): Promise<number> {
    await Promise.resolve(0);
    return n * 2;
}
function risky(n: number): number {
    if (n < 0) throw new Error("negative");
    return n;
}
async function main() {
    const shapes: Shape[] = [new Circle(1.5), new Square()];
    let total = 0;
    for (const s of shapes) total += s.area();
    const add = (x: number) => { counter += x; return counter; };
    add(3);
    const m = new Map<string, number>();
    m.set("one", 1);
    const [a, b] = [1, 2];
    try { risky(-1); } catch (e) { console.log(String(e)); }
    console.log(total, largest([3, 1, 4], (p, q) => p > q), kindOf("a"), Color.Green, Dir.Down, a + b);
    console.log(await later(21), new Derived().greet(), TABLE.length, m.get("one") ?? 0);
}
"#;

/// Encode `v`, check that encoding it again gives the same bytes, decode it and check that it
/// reads back whole and prints as `v` does (HIR types have no `PartialEq`). The encoded size and
/// the decoding time.
fn round_trip<T: Codec + std::fmt::Debug>(what: &str, v: &[T]) -> (usize, f64) {
    let encode = || {
        let mut w = Writer::new();
        w.uint(v.len() as u64);
        for x in v {
            x.enc(&mut w);
        }
        w.finish()
    };
    let (bytes, files) = encode();
    assert_eq!(
        encode(),
        (bytes.clone(), files.clone()),
        "{what}: deterministic"
    );
    let start = Instant::now();
    let mut r = Reader::new(&bytes, &files);
    let n = r.len();
    let out: Vec<T> = (0..n).map(|_| T::dec(&mut r)).collect();
    let secs = start.elapsed().as_secs_f64();
    assert!(r.done(), "{what}: read whole");
    assert_eq!(out.len(), v.len(), "{what}: count");
    for (i, (a, b)) in v.iter().zip(&out).enumerate() {
        let (a, b) = (format!("{a:?}"), format!("{b:?}"));
        assert!(a == b, "{what} #{i} changed:\n{a}\n---\n{b}");
    }
    (bytes.len(), secs)
}

fn check_program(p: &Program) {
    let (size, secs) = round_trip("def", &p.defs);
    eprintln!(
        "units::codec: {} defs ({} unchecked std stubs): {size} bytes, decoded in {:.2} ms",
        p.defs.len(),
        p.unchecked.len(),
        secs * 1e3
    );
    let (size, _) = round_trip("impl", &p.impls);
    eprintln!("units::codec: {} impls: {size} bytes", p.impls.len());
    let types: Vec<TyKind> = (0..p.types.len() as u32)
        .map(|i| p.types.kind(TyId(i)).clone())
        .collect();
    let (size, _) = round_trip("type", &types);
    eprintln!("units::codec: {} types: {size} bytes", types.len());
    // The element types of the program's tables.
    round_trip(
        "fn shape",
        &p.fn_shapes.values().cloned().collect::<Vec<_>>(),
    );
    round_trip(
        "fresh closure",
        &p.fresh_closures.values().copied().collect::<Vec<_>>(),
    );
    round_trip(
        "fn wrapper",
        &p.fn_wrappers.values().cloned().collect::<Vec<_>>(),
    );
    round_trip(
        "impl bounds",
        &p.extension_impls.values().cloned().collect::<Vec<_>>(),
    );
    round_trip(
        "cpu target",
        &p.cpu_targets.values().copied().collect::<Vec<_>>(),
    );
    let decorators: Vec<_> = p.decorators.iter().map(|(k, v)| (*k, v.clone())).collect();
    round_trip("decorators", &decorators);
    round_trip("c export", &p.c_exports);
    round_trip("module state", std::slice::from_ref(&p.module_state));
    let dyn_members: Vec<_> = p
        .dyn_members
        .iter()
        .flat_map(|m| m.values().cloned())
        .collect();
    round_trip("dyn members", &dyn_members);
}

#[test]
fn a_checked_program_s_hir_comes_back_as_it_was() {
    let p = programs::ok_src(SRC);
    assert!(p.defs.len() > 1000, "std's prelude is included");
    // Printing and decoding recurse as deep as the program's expressions nest.
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(crate::SEMA_STACK_BYTES)
            .spawn_scoped(s, || check_program(&p))
            .expect("test thread")
            .join()
            .expect("round trip")
    });
}

/// An expression nested `levels` deep (casts of a literal), encoded. Built, encoded and dropped
/// on a thread with sema's stack: the tests below measure the decoder alone.
fn nested_casts(levels: u32) -> (Vec<u8>, Vec<velt_common::FileId>) {
    use crate::hir::{Expr, ExprKind, Lit};
    let build = move || {
        let expr = |kind| Expr {
            kind,
            ty: TyId(0),
            span: velt_common::Span::DUMMY,
        };
        let mut e = expr(ExprKind::Lit(Lit::Int(1)));
        for _ in 0..levels {
            e = expr(ExprKind::Cast(Box::new(e)));
        }
        let mut w = Writer::new();
        e.enc(&mut w);
        w.finish()
    };
    std::thread::Builder::new()
        .stack_size(crate::SEMA_STACK_BYTES)
        .spawn(build)
        .expect("test thread")
        .join()
        .expect("encoded")
}

/// An expression as deep as a chain of 1,100 operands (`a + b + ...` nests a level per
/// operand) decodes on a thread that has sema's stack and says so, as the check of a program
/// does.
#[test]
fn hir_as_deep_as_a_long_operator_chain_decodes_on_sema_s_stack() {
    let (bytes, files) = nested_casts(1100);
    let done = std::thread::Builder::new()
        .stack_size(crate::SEMA_STACK_BYTES)
        .spawn(move || {
            super::with_decode_stack(crate::SEMA_STACK_BUDGET, || {
                let mut r = Reader::new(&bytes, &files);
                let e: crate::hir::Expr = Codec::dec(&mut r);
                // Dropped here, on the large stack.
                drop(e);
                r.done()
            })
        })
        .expect("test thread")
        .join()
        .expect("the decode returns");
    assert!(done);
}

/// HIR nested far deeper than an ordinary thread's stack holds is a failed decode there, not a
/// stack overflow.
#[test]
fn hir_nested_deeper_than_the_stack_holds_is_a_failed_decode() {
    let (bytes, files) = nested_casts(20_000);
    let done = std::thread::spawn(move || {
        let mut r = Reader::new(&bytes, &files);
        let e: crate::hir::Expr = Codec::dec(&mut r);
        // What came back is as deep as the decode got: left to the process, not dropped on
        // this stack.
        std::mem::forget(e);
        r.done()
    })
    .join()
    .expect("the decode returns");
    assert!(!done);
}

/// The HIR enums that have no variant without fields at tag 0 (listed `@fielded`) end from
/// zeros: none holds itself in a box there, so damaged input ends in each.
#[test]
fn hir_enums_end_on_damaged_input() {
    use crate::hir::*;
    macro_rules! ends {
        ($($t:ty),* $(,)?) => {$(
            assert!(super::ends_from_zeros::<$t>(), stringify!($t));
        )*};
    }
    ends!(Def, CTy, HostTy, CValue, StmtKind, ExprKind, Callee, Lit, TyKind, LitValue, TyOp);
    ends!(
        FnWrapped,
        FreshClosure,
        DecoratorTarget,
        Expr,
        Stmt,
        Pat,
        Block
    );
}

/// A tag stays with its intrinsic for good: `IsUnique` (#3318) took the next free one, 213, and
/// reads back as itself.
#[test]
fn is_unique_is_intrinsic_tag_213() {
    use crate::hir::Intrinsic;
    let mut w = Writer::new();
    Intrinsic::IsUnique.enc(&mut w);
    assert_eq!(w.out, [213]);
    round_trip("intrinsics", &[Intrinsic::IsUnique, Intrinsic::SymbolAt]);
}
