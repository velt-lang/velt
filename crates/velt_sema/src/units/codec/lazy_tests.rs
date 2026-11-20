//! A definition table decoded on demand reads as the table it was encoded from, on every read
//! path; an entry is decoded with the decode stack its unit was opened with; and a damaged entry
//! marks its unit damaged instead of stopping the check.

use std::rc::Rc;

use velt_common::{FileId, Span};

use super::defs_tests::{check_by_units, inspect_once, kept, shown};
use super::hook::Hook;
use super::{with_decode_stack, Arena, Codec, Reader, Writer};
use crate::ctx::{Ctx, DefTable};
use crate::defs::DefInfo;
use crate::hir::{Expr, ExprKind, Lit, TyId};

/// `t` encoded, as a unit's bytes and files.
fn encoded<T: Codec>(t: &DefTable<T>) -> (Rc<Vec<u8>>, Rc<Vec<FileId>>) {
    let mut w = Writer::new();
    t.enc(&mut w);
    let (bytes, files) = w.finish();
    (Rc::new(bytes), Rc::new(files))
}

/// Each read path of a table decoded on demand, on a fresh table each: the entries it gives,
/// printed. The eager table's entries come first.
fn read_paths(cx: &Ctx) -> Vec<(&'static str, Vec<String>)> {
    let view = Arena::default();
    let eager: Vec<String> = cx.info.iter().map(|i| shown(kept(i, &view))).collect();
    let (bytes, files) = encoded(&cx.info);
    let arena = Arena::default();
    let fresh = || {
        // SAFETY: every value decoded here is dropped in this function, before `arena`.
        let mut r = unsafe { Reader::over_unit(&bytes, 0, &files, &arena) };
        let t: DefTable<DefInfo> = Codec::dec(&mut r);
        assert!(r.done(), "the table's framing decodes");
        t
    };
    let n = eager.len();
    let all = |v: Vec<&DefInfo>| v.into_iter().map(|i| shown(i.clone())).collect::<Vec<_>>();
    let mut out = vec![("eager", eager)];
    let t = fresh();
    out.push(("index", all((0..n).map(|i| &t[i]).collect())));
    let t = fresh();
    out.push((
        "get",
        all((0..n).map(|i| t.get(i).expect("in range")).collect()),
    ));
    let t = fresh();
    out.push(("iter", all(t.iter().collect())));
    let t = fresh();
    let mut back: Vec<&DefInfo> = t.iter().rev().collect();
    back.reverse();
    out.push(("iter backwards", all(back)));
    let t = fresh();
    out.push(("items_all", all(t.items_all().collect())));
    let t = fresh();
    let got: Vec<String> = (0..n).map(|i| shown(t[i].clone())).collect();
    let mut m = fresh();
    let by_mut: Vec<String> = (0..n)
        .map(|i| {
            let e: &mut DefInfo = &mut m[i];
            shown(e.clone())
        })
        .collect();
    out.push(("index_mut", by_mut));
    out.push(("index again", got));
    let mut t2 = fresh();
    let by_get_mut: Vec<String> = (0..n)
        .map(|i| shown(t2.get_mut(i).expect("in range").clone()))
        .collect();
    out.push(("get_mut", by_get_mut));
    let mut t3 = fresh();
    out.push((
        "iter_mut",
        t3.iter_mut().map(|i| shown(i.clone())).collect(),
    ));
    out.push((
        "into_vec",
        fresh().into_vec().into_iter().map(shown).collect(),
    ));
    let c = fresh().clone();
    out.push(("clone", all((0..n).map(|i| &c[i]).collect())));
    drop(t);
    out
}

/// std's definition records, as a table decoded on demand, read on every path: each gives what
/// the table it was encoded from has (functions' bodies aside, which a unit does not keep).
#[test]
fn lazy_tables_read_as_eager_ones_on_every_path() {
    let (inspect, found) = inspect_once(read_paths);
    let hook = Hook {
        inspect: Some(inspect),
    };
    let (program, diags) = check_by_units(super::hir_tests::SRC, hook);
    assert!(program.is_some(), "the program checks: {diags:?}");
    let paths = found
        .lock()
        .unwrap()
        .take()
        .expect("std's unit was shown to the hook");
    let eager = &paths[0].1;
    assert!(eager.len() > 1000, "std's definitions are there");
    for (path, got) in &paths[1..] {
        assert_eq!(got.len(), eager.len(), "{path}");
        let first = (got.iter().zip(eager)).position(|(a, b)| a != b);
        assert!(first.is_none(), "{path}: entry {first:?} differs");
    }
}

/// An expression `levels` casts deep.
fn deep(levels: u32) -> Expr {
    let expr = |kind| Expr {
        kind,
        ty: TyId(0),
        span: Span::DUMMY,
    };
    let mut e = expr(ExprKind::Lit(Lit::Int(1)));
    for _ in 0..levels {
        e = expr(ExprKind::Cast(Box::new(e)));
    }
    e
}

/// A unit opened with sema's decode stack decodes an entry deeper than an ordinary stack holds
/// when the check reads it later, outside the scope that opened it.
#[test]
fn an_entry_is_decoded_with_the_stack_its_unit_was_opened_with() {
    let run = || {
        let t: DefTable<Expr> = vec![deep(0), deep(3000)].into();
        let (bytes, files) = encoded(&t);
        drop(t);
        let arena = Arena::default();
        let (t, src) = with_decode_stack(crate::SEMA_STACK_BUDGET, || {
            // SAFETY: what is decoded is dropped in this closure's caller, before `arena`.
            let mut r = unsafe { Reader::over_unit(&bytes, 0, &files, &arena) };
            let t: DefTable<Expr> = Codec::dec(&mut r);
            assert!(r.done());
            (t, r.src.clone().expect("a unit's reader"))
        });
        // Read here, outside the decode stack's scope.
        let mut levels = 0;
        let mut e = &t[1];
        while let ExprKind::Cast(inner) = &e.kind {
            levels += 1;
            e = inner;
        }
        let damaged = src.damaged();
        drop(t);
        (levels, damaged)
    };
    let (levels, damaged) = std::thread::Builder::new()
        .stack_size(crate::SEMA_STACK_BYTES)
        .spawn(run)
        .expect("test thread")
        .join()
        .expect("the entry decodes");
    assert_eq!((levels, damaged), (3000, false));
}

/// An entry whose bytes are damaged inside an intact table is read as a stand-in and marks its
/// unit damaged; the check goes on. An empty entry is refused with the table.
#[test]
fn a_damaged_entry_marks_its_unit_and_does_not_stop_the_check() {
    let t: DefTable<String> = vec!["first".to_string(), "second".to_string()].into();
    let (bytes, files) = encoded(&t);
    let arena = Arena::default();
    // The second entry's length byte, stated larger than its bytes.
    let at = (bytes.windows(7))
        .position(|w| w == b"\x06second")
        .expect("the entry");
    let mut damaged = (*bytes).clone();
    damaged[at] = 0x7f;
    let damaged = Rc::new(damaged);
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::over_unit(&damaged, 0, &files, &arena) };
    let t: DefTable<String> = Codec::dec(&mut r);
    assert!(r.done(), "the framing is intact");
    let src = r.src.clone().expect("a unit's reader");
    assert_eq!(t[0], "first");
    assert!(!src.damaged());
    let _stand_in = &t[1];
    assert!(src.damaged());
    // Two entries of one byte each; the first's end moved to the table's start makes it empty
    // and the second two bytes long, which the framing's total still allows.
    let two: DefTable<String> = vec![String::new(), String::new()].into();
    let (bytes, files) = encoded(&two);
    let mut empty = (*bytes).clone();
    let ends = empty.len() - 2;
    assert_eq!(&empty[ends..], &[1, 2]);
    empty[ends] = 0;
    let empty = Rc::new(empty);
    // SAFETY: as above.
    let mut r = unsafe { Reader::over_unit(&empty, 0, &files, &arena) };
    let _: DefTable<String> = Codec::dec(&mut r);
    assert!(!r.done(), "an empty entry is refused");
}
