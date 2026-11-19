use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{FileId, Span};

use super::*;
use crate::hir::{DefId, TyId};

fn round_trip<T: Codec + PartialEq + std::fmt::Debug>(v: &T) -> T {
    let mut w = Writer::new();
    v.enc(&mut w);
    let (bytes, files) = w.finish();
    let mut r = Reader::new(&bytes, &files);
    let out = T::dec(&mut r);
    assert!(r.done(), "a whole value was read");
    out
}

#[test]
fn values_come_back_as_they_were_encoded() {
    let v: Vec<(u32, String, Option<i64>)> = vec![
        (0, String::new(), None),
        (300, "naïve".into(), Some(-5)),
        (u32::MAX, "x".repeat(200), Some(i64::MIN)),
    ];
    assert_eq!(round_trip(&v), v);
    let mut m: FxHashMap<DefId, Vec<TyId>> = FxHashMap::default();
    m.insert(DefId(7), vec![TyId(1), TyId(2)]);
    m.insert(DefId(3), vec![]);
    assert_eq!(round_trip(&m), m);
    let s: FxHashSet<u32> = [5, 1, 9].into_iter().collect();
    assert_eq!(round_trip(&s), s);
    assert_eq!(round_trip(&1.5f64).to_bits(), 1.5f64.to_bits());
}

/// Spans keep their files through the unit's table, mapped to the decoding session's ids.
#[test]
fn spans_are_decoded_in_the_session_s_files() {
    let spans = vec![
        Span::new(FileId(40), 3, 9),
        Span::DUMMY,
        Span::new(FileId(12), 0, 0),
        Span::new(FileId(40), 100, 120),
    ];
    let mut w = Writer::new();
    spans.enc(&mut w);
    let (bytes, files) = w.finish();
    assert_eq!(files, vec![FileId(40), FileId(12)]);
    // Another session added the same files under other ids.
    let session = [FileId(2), FileId(5)];
    let mut r = Reader::new(&bytes, &session);
    let out: Vec<Span> = Codec::dec(&mut r);
    assert!(r.done());
    let want = vec![
        Span::new(FileId(2), 3, 9),
        Span::DUMMY,
        Span::new(FileId(5), 0, 0),
        Span::new(FileId(2), 100, 120),
    ];
    assert_eq!(out, want);
}

/// A cut or damaged value is a failed decode, never a panic or a huge allocation.
#[test]
fn damaged_values_fail_to_decode() {
    let v: Vec<String> = vec!["abc".into(); 50];
    let mut w = Writer::new();
    v.enc(&mut w);
    let (bytes, files) = w.finish();
    for cut in [0, 1, 7, bytes.len() / 2, bytes.len() - 1] {
        let mut r = Reader::new(&bytes[..cut], &files);
        let _: Vec<String> = Codec::dec(&mut r);
        assert!(!r.done(), "cut at {cut}");
    }
    // A length far beyond the bytes left.
    let huge = [0xff, 0xff, 0xff, 0xff, 0x0f];
    let mut r = Reader::new(&huge, &files);
    let _: Vec<u32> = Codec::dec(&mut r);
    assert!(!r.done());
}

/// A reference to an AST node is stored once however often it is written, without a function's
/// body, and comes back as a node of the reader's arena.
#[test]
fn ast_references_come_back_as_nodes_of_the_arena() {
    use velt_syntax::ast::ItemKind;
    let src = "function f(a: number): number {\n  return a + 1;\n}\ntype A = { x: number };\n";
    let (module, diags) = velt_syntax::parse_file(FileId(3), src);
    assert!(diags.is_empty(), "{diags:?}");
    let (f, alias) = match (&module.items[0].kind, &module.items[1].kind) {
        (ItemKind::Function(f), ItemKind::TypeAlias(a)) => (f, a),
        _ => panic!("a function and a type alias"),
    };
    assert!(!f.body.stmts.is_empty());
    let mut w = Writer::new();
    (f, alias, f).enc(&mut w);
    let (bytes, files) = w.finish();
    let arena = Arena::default();
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::with_arena(&bytes, &files, &arena) };
    type Refs<'x> = (
        &'x velt_syntax::ast::FnDecl,
        &'x velt_syntax::ast::TypeAlias,
        &'x velt_syntax::ast::FnDecl,
    );
    let (f1, alias1, f2): Refs = Codec::dec(&mut r);
    assert!(r.done(), "a whole value was read");
    assert!(std::ptr::eq(f1, f2), "one node for both references");
    assert_eq!(arena.len(), 2, "each node is kept once");
    assert_eq!(format!("{:?}", f1.sig), format!("{:?}", f.sig));
    assert!(f1.body.stmts.is_empty(), "a unit keeps no body");
    assert_eq!(format!("{alias1:?}"), format!("{alias:?}"));
    // Without an arena there is nowhere to keep a node: the decode fails.
    let mut r = Reader::new(&bytes, &files);
    let _: Refs = Codec::dec(&mut r);
    assert!(!r.done());
}
