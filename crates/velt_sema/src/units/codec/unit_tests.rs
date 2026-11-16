//! std's frozen unit comes back from its encoding with every field it saves as it was, and the
//! app checked against the decoded unit is checked as against the unit itself.

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
        && (0..table.len() as u32).all(|i| table.get(table.kind(TyId(i))) == Some(TyId(i)));
    let frozen = decoded.unit_base == cx.unit_base
        && (0..decoded.defs.len()).all(|d| decoded.is_frozen(crate::hir::DefId(d as u32)));
    Found {
        sizes,
        total: bytes.len(),
        decode_ms,
        differs,
        deterministic,
        tables_ok: types_ok && frozen && again == bytes,
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

/// Small programs by units: std's unit through the codec, then the app against it.
#[test]
fn std_s_unit_comes_back_as_it_was_and_checks_the_app_the_same() {
    for src in [super::hir_tests::SRC, LEANS_ON_STD] {
        unit_round_trip(src);
    }
}

fn unit_round_trip(src: &str) {
    let (inspect, found) = inspect_once(round_trip);
    let in_memory = check_by_units(
        src,
        Hook {
            inspect: Some(inspect),
            decoded: false,
        },
    );
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
    // The app's stage from the decoded unit.
    let decoded = check_by_units(
        src,
        Hook {
            inspect: None,
            decoded: true,
        },
    );
    let (Some(a), Some(b)) = (&in_memory.0, &decoded.0) else {
        panic!(
            "both checks build a program: {:?} / {:?}",
            in_memory.1, decoded.1
        );
    };
    let (a, b) = (
        (a.clone(), in_memory.1.clone()),
        (b.clone(), decoded.1.clone()),
    );
    if let Some(what) = crate::program::differs(&a, &b) {
        panic!("the app checked against the decoded unit differs: {what}");
    }
}
