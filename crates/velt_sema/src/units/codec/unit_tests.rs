//! std's frozen unit comes back from its encoding with every field it saves as it was.

use std::time::Instant;

use super::defs_tests::{check_by_units, inspect_once};
use super::hook::Hook;
use super::unit::encode_unit_sized;
use super::{decode_unit, Arena};
use crate::ctx::Ctx;
use crate::hir::TyId;

/// What the round trip of std's unit found.
struct Found {
    sizes: Vec<(&'static str, usize)>,
    total: usize,
    decode_ms: f64,
    /// The first saved field whose encoding differs after the round trip, with the bytes there.
    differs: Option<String>,
    deterministic: bool,
    /// The decoded type table's intern map gives every type its id; the definitions are frozen.
    tables_ok: bool,
}

/// Where each field is in an encoding (`sizes` in order, after the header and file table).
fn field_ranges(
    total: usize,
    sizes: &[(&'static str, usize)],
) -> Vec<(&'static str, usize, usize)> {
    let sizes = &sizes[..sizes.len() - 1];
    let fields: usize = sizes.iter().map(|s| s.1).sum();
    let mut at = total - fields;
    (sizes.iter())
        .map(|&(n, len)| {
            at += len;
            (n, at - len, at)
        })
        .collect()
}

fn round_trip(cx: &Ctx) -> Found {
    let (bytes, sizes) = encode_unit_sized(cx);
    let deterministic = encode_unit_sized(cx).0 == bytes;
    let arena = Arena::default();
    let start = Instant::now();
    // SAFETY: the decoded context is dropped in this function, before `arena`.
    let bytes = std::rc::Rc::new(bytes);
    let decoded = unsafe { decode_unit(&bytes, &arena, cx.modules.clone()) };
    let decode_ms = start.elapsed().as_secs_f64() * 1e3;
    let decoded = decoded.expect("std's unit decodes whole");
    let (again, again_sizes) = encode_unit_sized(&decoded);
    let mut differs = None;
    let (a, b) = (
        field_ranges(bytes.len(), &sizes),
        field_ranges(again.len(), &again_sizes),
    );
    for (&(name, lo, hi), &(_, lo2, hi2)) in a.iter().zip(&b) {
        if bytes[lo..hi] != again[lo2..hi2] {
            let at = (bytes[lo..hi].iter().zip(&again[lo2..hi2])).position(|(x, y)| x != y);
            differs = Some(format!(
                "{name}: {} bytes, {} after the round trip, first difference at {at:?}",
                hi - lo,
                hi2 - lo2
            ));
            break;
        }
    }
    let table = &decoded.ty.table;
    let types_ok = table.len() == cx.ty.table.len()
        && (0..table.len() as u32).all(|i| {
            // A read-only array is a twin of `T[]`: its kind names `T[]`.
            decoded.ty.is_readonly_array(TyId(i)) == cx.ty.is_readonly_array(TyId(i))
                && (decoded.ty.is_readonly_array(TyId(i))
                    || table.get(table.kind(TyId(i))) == Some(TyId(i)))
        });
    let frozen = (0..decoded.defs.len()).all(|d| decoded.is_frozen(crate::hir::DefId(d as u32)));
    Found {
        sizes,
        total: bytes.len(),
        decode_ms,
        differs,
        deterministic,
        tables_ok: types_ok && frozen && again == *bytes,
    }
}

/// A program that leans on std's declarations from the app's stage: a subclass of a std class,
/// std's generic aliases and utility types, `typeof` of std functions, std's statics and
/// interfaces implemented by the app.
const LEANS_ON_STD: &str = r#"
class AppError extends Error {
    constructor(public code: number) { super(`code ${code}`); }
}
type Scores = Record<string, number>;
type Opt = Partial<{ a: number; b: string }>;
function total(s: Scores): number {
    let t = 0;
    for (const k of Object.keys(s)) t += s[k];
    return t;
}
class Counter implements Iterable<number> {
    constructor(private n: number) {}
    *[Symbol.iterator](): Iterator<number> { for (let i = 0; i < this.n; i++) yield i; }
}
async function main() {
    const o: Opt = { a: 1 };
    const m = new Map<string, number[]>([["x", [3, 1, 2]]]);
    const sorted = Array.from(m.values()).map((xs) => xs.toSorted((p, q) => p - q));
    let r: ReturnType<typeof total> = total({ x: 1, y: 2 });
    try { throw new AppError(7); } catch (e) { if (e instanceof AppError) r += e.code; }
    const all = await Promise.all([Promise.resolve(1), Promise.resolve(2)]);
    console.log(JSON.stringify(o), sorted, r, all, [...new Counter(3)], Math.max(...all));
}
"#;

/// Small programs by units: std's frozen unit through the codec.
#[test]
fn std_s_unit_comes_back_as_it_was() {
    for src in [super::hir_tests::SRC, LEANS_ON_STD] {
        unit_round_trip(src);
    }
}

fn unit_round_trip(src: &str) {
    let (inspect, found) = inspect_once(round_trip);
    let (program, diags) = check_by_units(
        src,
        Hook {
            inspect: Some(inspect),
        },
    );
    assert!(program.is_some(), "the program checks: {diags:?}");
    let found = found
        .lock()
        .unwrap()
        .take()
        .expect("std's unit was shown to the hook");
    assert!(
        found.deterministic,
        "the same unit encodes to the same bytes"
    );
    if let Some(d) = &found.differs {
        panic!("a saved field changed in the round trip: {d}");
    }
    assert!(found.tables_ok, "the type table and the freeze are rebuilt");
    let mut sizes = found.sizes.clone();
    sizes.sort_by_key(|s| std::cmp::Reverse(s.1));
    let shown: Vec<String> = (sizes.iter().take(16))
        .map(|(n, s)| format!("{n} {s}"))
        .collect();
    eprintln!(
        "units::codec: std's unit {} bytes, decoded in {:.1} ms; largest fields: {}",
        found.total,
        found.decode_ms,
        shown.join(", ")
    );
}

/// What the checks of std's frozen unit below found.
struct Checked {
    /// Positions where a byte of the encoded unit was changed, and how many of those still
    /// decoded.
    flipped: usize,
    decoded_damaged: usize,
    /// How deep the expression put in the unit came back (3,000 casts).
    deep_levels: usize,
    /// The fields the unit does not save that are not as a fresh context has them.
    empty_differing: Vec<String>,
}

fn checks(cx: &Ctx) -> Checked {
    // Damaged bytes: every position in steps, past the format.
    let (bytes, _) = encode_unit_sized(cx);
    let step = (bytes.len() / 300).max(1);
    let (mut flipped, mut decoded_damaged) = (0, 0);
    for at in (super::unit::FORMAT.len()..bytes.len()).step_by(step) {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x5a;
        let damaged = std::rc::Rc::new(damaged);
        let arena = Arena::default();
        // SAFETY: the decoded context, if any, is dropped here, before `arena`.
        let out = unsafe { decode_unit(&damaged, &arena, cx.modules.clone()) };
        flipped += 1;
        decoded_damaged += usize::from(out.is_some());
    }
    // An expression deeper than 1 MiB of decode stack holds, in the unit's definitions.
    let mut deep = cx.clone();
    deep.thaw_defs();
    let mut init = crate::hir::Expr {
        kind: crate::hir::ExprKind::Lit(crate::hir::Lit::Int(1)),
        ty: TyId(0),
        span: velt_common::Span::DUMMY,
    };
    for _ in 0..3000 {
        init = crate::hir::Expr {
            kind: crate::hir::ExprKind::Cast(Box::new(init)),
            ty: TyId(0),
            span: velt_common::Span::DUMMY,
        };
    }
    deep.defs
        .push(Some(crate::hir::Def::Global(crate::hir::GlobalDef {
            name: "deep".into(),
            ty: TyId(0),
            init,
            span: velt_common::Span::DUMMY,
            is_slot: false,
            c_const: None,
            c_global: None,
        })));
    let last = deep.defs.len() - 1;
    let (bytes, _) = encode_unit_sized(&deep);
    drop(deep);
    let bytes = std::rc::Rc::new(bytes);
    let arena = Arena::default();
    // SAFETY: the decoded context is dropped here, before `arena`.
    let back =
        unsafe { decode_unit(&bytes, &arena, cx.modules.clone()) }.expect("the unit decodes");
    let mut deep_levels = 0;
    if let Some(crate::hir::Def::Global(g)) = &back.defs[last] {
        let mut e = &g.init;
        while let crate::hir::ExprKind::Cast(inner) = &e.kind {
            deep_levels += 1;
            e = inner;
        }
    }
    drop(back);
    // The fields a unit leaves out as empty are empty at its end.
    let fresh = Ctx::over(cx.modules.clone(), 0);
    let empty_differing = (super::unit::empty_fields_differing(cx, &fresh).into_iter())
        .map(|(f, a, b)| format!("{f}: {a} at the unit's end, {b} fresh"))
        .collect();
    Checked {
        flipped,
        decoded_damaged,
        deep_levels,
        empty_differing,
    }
}

/// std's unit: a damaged copy is refused whole (its checksum), an expression 3,000 levels
/// deep in it decodes through `decode_unit` (sema's decode stack), and every field the unit
/// leaves out as empty is as a fresh context has it.
#[test]
fn std_s_unit_is_refused_damaged_decodes_deep_and_leaves_out_only_empty_state() {
    let (inspect, found) = inspect_once(checks);
    let (program, diags) = check_by_units(
        super::hir_tests::SRC,
        Hook {
            inspect: Some(inspect),
        },
    );
    assert!(program.is_some(), "the program checks: {diags:?}");
    let c = found
        .lock()
        .unwrap()
        .take()
        .expect("std's unit was shown to the hook");
    assert!(c.flipped >= 250, "{} positions", c.flipped);
    assert_eq!(c.decoded_damaged, 0, "of {} damaged copies", c.flipped);
    assert_eq!(c.deep_levels, 3000);
    assert!(
        c.empty_differing.is_empty(),
        "{}",
        c.empty_differing.join("\n")
    );
}
