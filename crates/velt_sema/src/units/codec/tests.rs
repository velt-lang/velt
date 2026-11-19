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
        // An end before the start is kept as it is.
        Span::new(FileId(12), 10, 3),
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
        Span::new(FileId(5), 10, 3),
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

/// A reference that names a node of another type is a failed decode; the node standing in for
/// it is kept in the reader's arena, as a decoded node is.
#[test]
fn a_damaged_ast_reference_fails_and_its_placeholder_is_the_arena_s() {
    use velt_syntax::ast::{FnDecl, ItemKind, TypeAlias};
    let (module, _) = velt_syntax::parse_file(FileId(3), "function f(): number { return 1; }\n");
    let ItemKind::Function(f) = &module.items[0].kind else {
        panic!("a function");
    };
    let mut w = Writer::new();
    f.enc(&mut w);
    let (bytes, files) = w.finish();
    let arena = Arena::default();
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::with_arena(&bytes, &files, &arena) };
    let alias: &TypeAlias = Codec::dec(&mut r);
    assert!(!r.done(), "a function is not a type alias");
    assert!(!alias.name.name.is_empty(), "a whole node stands in");
    assert_eq!(arena.len(), 1, "the node standing in is the arena's");
    // The same bytes as what they are.
    let mut r = unsafe { Reader::with_arena(&bytes, &files, &arena) };
    let _: &FnDecl = Codec::dec(&mut r);
    assert!(r.done());
    assert_eq!(arena.len(), 2);
}

#[derive(Debug, PartialEq)]
enum Shape {
    Dot,
    Line(u32),
    Rect { w: u32, h: u32 },
}

enums! {
    Shape {
        0 => Dot,
        1 => Line(a),
        2 => Rect { w, h },
    }
}

/// An enum comes back by its tag; a tag no variant has is a failed decode that ends at the
/// variant with tag 0 (which `enums!` requires).
#[test]
fn an_unknown_tag_is_a_failed_decode() {
    for v in [Shape::Dot, Shape::Line(7), Shape::Rect { w: 2, h: 300 }] {
        assert_eq!(round_trip(&v), v);
    }
    let mut w = Writer::new();
    Shape::Line(7).enc(&mut w);
    let (mut bytes, files) = w.finish();
    let tag = bytes.len() - 2;
    assert_eq!(bytes[tag], 1);
    bytes[tag] = 9;
    let mut r = Reader::new(&bytes, &files);
    assert_eq!(Shape::dec(&mut r), Shape::Dot);
    assert!(!r.done());
    // The tag alone fails the reader: what follows it would decode as a whole value.
    let mut r = Reader::bare(&[9, 0], &[]);
    assert_eq!(Shape::dec(&mut r), Shape::Dot);
    assert!(r.failed());
}

/// A value that nests: a list of itself.
#[derive(Debug, PartialEq, Default)]
struct Nest(Vec<Nest>);

tuples! {
    Nest(a)
}

/// How many `Nest`s `n` is made of.
fn nests(n: &Nest) -> usize {
    1 + n.0.iter().map(nests).sum::<usize>()
}

/// The bytes of capacity the lists of `n` hold.
fn capacity(n: &Nest) -> usize {
    n.0.capacity() * std::mem::size_of::<Nest>() + n.0.iter().map(capacity).sum::<usize>()
}

/// Nested lists that each state as many elements as there are bytes left, of which only the
/// first exists: what is decoded stays in proportion to the input (no list is filled with
/// stand-ins once the value is known to be damaged), and so does the capacity its lists hold.
#[test]
fn stated_lengths_of_a_damaged_value_are_not_filled_in_or_reserved() {
    const LEN: usize = 4000;
    const LEVELS: usize = 500;
    let mut bytes = vec![];
    for _ in 0..LEVELS {
        // The bytes left after this length, as a two-byte varint.
        let n = LEN - bytes.len() - 2;
        bytes.push((n & 0x7f) as u8 | 0x80);
        bytes.push((n >> 7) as u8);
    }
    bytes.resize(LEN, 0);
    let mut r = Reader::bare(&bytes, &[]);
    let v = Nest::dec(&mut r);
    assert!(!r.done());
    let made = nests(&v);
    assert!(made <= LEN, "{made} values from {LEN} bytes");
    // What the lists reserved before their elements were read is within the reader's allowance;
    // the rest of their capacity is what the elements they hold made them grow to.
    let allowed = LEN * RESERVE_PER_BYTE + 4096 + 4 * made * std::mem::size_of::<Nest>();
    let held = capacity(&v);
    assert!(
        held <= allowed,
        "{held} bytes of capacity, {allowed} allowed"
    );
}

/// A list nested `levels` deep (one element per level, the innermost empty), decoded on the
/// current thread: whether it decoded, and how many levels came back. Taken apart level by
/// level, so that only the decoder's own use of the stack is tested.
fn decode_nested(levels: usize) -> (bool, usize) {
    let mut bytes = vec![1u8; levels - 1];
    bytes.push(0);
    let mut r = Reader::bare(&bytes, &[]);
    let mut v = Nest::dec(&mut r);
    let mut depth = 1;
    while let Some(inner) = v.0.pop() {
        v = inner;
        depth += 1;
    }
    (r.done(), depth)
}

/// On an ordinary thread, a value nested deeper than its stack holds is a failed decode, not a
/// stack overflow; one nested a few thousand levels decodes there.
#[test]
fn a_value_nested_deeper_than_the_stack_holds_is_a_failed_decode() {
    let on_a_thread = |levels: usize| {
        std::thread::spawn(move || decode_nested(levels))
            .join()
            .expect("the decode returns")
    };
    assert_eq!(on_a_thread(2000), (true, 2000));
    let (done, depth) = on_a_thread(1_000_000);
    assert!(!done);
    assert!(depth <= MAX_DEPTH as usize + 1, "{depth} levels came back");
}

/// A thread that says how much stack its decoders have ([`with_decode_stack`]) decodes a value
/// nested to [`MAX_DEPTH`]; one level deeper is a failed decode.
#[test]
fn a_thread_with_a_large_stack_decodes_to_the_nesting_limit() {
    let limit = MAX_DEPTH as usize;
    let on_sema_s_stack = move |levels: usize| {
        std::thread::Builder::new()
            .stack_size(crate::SEMA_STACK_BYTES)
            .spawn(move || with_decode_stack(crate::SEMA_STACK_BUDGET, || decode_nested(levels)))
            .expect("test thread")
            .join()
            .expect("the decode returns")
    };
    assert_eq!(on_sema_s_stack(limit), (true, limit));
    assert_eq!(on_sema_s_stack(limit + 1), (false, limit + 1));
}

/// Every value encoding at its edge values.
#[test]
fn every_value_encoding_round_trips_at_its_edges() {
    use std::cell::{OnceCell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;
    macro_rules! same {
        ($($v:expr),* $(,)?) => {$( let v = $v; assert_eq!(round_trip(&v), v); )*};
    }
    same!(
        true,
        false,
        0u8,
        255u8,
        0u32,
        u32::MAX,
        0u64,
        127u64,
        128u64,
        u64::MAX
    );
    same!(0usize, usize::MAX, i64::MIN, -1i64, 0i64, i64::MAX);
    same!(0u128, u128::MAX, i128::MIN, -1i128, 0i128, i128::MAX);
    same!(
        String::new(),
        "naïve ✓".to_string(),
        Rc::<str>::from(""),
        Rc::<str>::from("rc ✓")
    );
    same!(
        FileId(0),
        FileId(7),
        DefId(u32::MAX),
        TyId(3),
        crate::hir::LocalId(9)
    );
    same!([0u8; 0], [1u8, 2, 3, 255]);
    same!(
        None::<u32>,
        Some(5u32),
        Some(Box::new(7u64)),
        None::<Box<u64>>,
        Box::new("b".to_string())
    );
    same!(RefCell::new(4u32), vec![3u32, 1, 2], Vec::<String>::new());
    // Fields of one type, so that an order swapped on decode shows.
    same!(
        (1u32, 2u32),
        (1u32, 2u32, 3u32),
        (1u32, 2u32, 3u32, 4u32),
        (true, "s".to_string(), 9u64)
    );
    same!(
        BTreeMap::from([(1u32, 2u32), (3, 4)]),
        BTreeSet::from([5u32, 1, 9])
    );
    let set: OnceCell<u32> = OnceCell::new();
    set.set(6).expect("empty");
    same!(set, OnceCell::<u32>::new());
    // Floats by their bits: a NaN with a payload, the sign of zero.
    for bits in [
        0x7ff8_dead_beef_0001u64,
        (-0.0f64).to_bits(),
        f64::INFINITY.to_bits(),
        1.5f64.to_bits(),
    ] {
        assert_eq!(round_trip(&f64::from_bits(bits)).to_bits(), bits);
    }
}

/// The same map or set encodes to the same bytes whatever order it was filled in.
#[test]
fn a_map_s_encoding_does_not_depend_on_how_it_was_filled() {
    let bytes = |v: &dyn Fn(&mut Writer)| {
        let mut w = Writer::new();
        v(&mut w);
        w.finish().0
    };
    let mut a: FxHashMap<u32, String> = FxHashMap::default();
    let mut b: FxHashMap<u32, String> =
        FxHashMap::with_capacity_and_hasher(4096, Default::default());
    let mut sa: FxHashSet<DefId> = FxHashSet::default();
    let mut sb: FxHashSet<DefId> = FxHashSet::with_capacity_and_hasher(4096, Default::default());
    for i in 0..300u32 {
        a.insert(i * 7919, format!("v{i}"));
        sa.insert(DefId(i * 31));
    }
    for i in (0..300u32).rev() {
        b.insert(i * 7919, format!("v{i}"));
        sb.insert(DefId(i * 31));
    }
    assert_eq!(bytes(&|w| a.enc(w)), bytes(&|w| b.enc(w)));
    assert_eq!(bytes(&|w| sa.enc(w)), bytes(&|w| sb.enc(w)));
    // In key order: the first entry written is the smallest key's.
    let (first, _) = round_trip(&a).into_iter().min().expect("entries");
    assert_eq!(first, 0);
}

/// A length is at most the bytes left, also for elements that take no bytes; a varint longer
/// than its type is refused.
#[test]
fn lengths_and_varints_are_checked() {
    // A million elements of no bytes each, stated in three bytes.
    let mut bytes = vec![];
    push_uint(&mut bytes, 1_000_000);
    let mut r = Reader::bare(&bytes, &[]);
    let v: Vec<[u8; 0]> = Codec::dec(&mut r);
    assert!(v.is_empty() && !r.done(), "{} elements", v.len());
    // u64: ten bytes at most, the tenth 0 or 1.
    let max = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
    let mut r = Reader::bare(&max, &[]);
    assert_eq!(u64::dec(&mut r), u64::MAX);
    assert!(r.done());
    let mut over = max;
    over[9] = 0x02;
    let mut eleven = [0x80u8; 11];
    eleven[10] = 0x01;
    for bad in [&over[..], &eleven[..]] {
        let mut r = Reader::bare(bad, &[]);
        u64::dec(&mut r);
        assert!(r.failed(), "{bad:x?}");
    }
    // u128: nineteen bytes at most, the last at most 3.
    let mut w = Writer::new();
    u128::MAX.enc(&mut w);
    let mut top = w.out.clone();
    assert_eq!((top.len(), top[18]), (19, 0x03));
    top[18] = 0x04;
    let mut r = Reader::bare(&top, &[]);
    u128::dec(&mut r);
    assert!(r.failed());
}

/// A list nested in itself, with a leaf: damaged input ends at the leaf.
#[derive(Debug, PartialEq)]
enum Deep {
    Leaf,
    Wrap(Box<Deep>),
}

enums! {
    Deep {
        0 => Leaf,
        1 => Wrap(a),
    }
}

/// An enum that has no variant without fields, listed `@fielded`: its tag 0 ends from zeros.
#[derive(Debug, PartialEq)]
enum Pair {
    Of(u32, Vec<Pair>),
    Boxed(Box<Pair>),
}

enums! {
    @fielded Pair {
        0 => Of(a, b),
        1 => Boxed(a),
    }
}

/// An enum that holds itself decodes damaged input to an end: tag 0 has no fields (or, for an
/// enum listed `@fielded`, fields that end from zeros), which `enums!` requires.
#[test]
fn a_recursive_enum_ends_on_damaged_input() {
    assert!(ends_from_zeros::<Deep>());
    assert!(ends_from_zeros::<Pair>());
    // Three levels, then a tag no variant has.
    let mut r = Reader::bare(&[1, 1, 1, 9, 1, 1], &[]);
    let v = Deep::dec(&mut r);
    assert!(r.failed());
    let leaf = Deep::Wrap(Box::new(Deep::Wrap(Box::new(Deep::Wrap(Box::new(
        Deep::Leaf,
    ))))));
    assert_eq!(v, leaf);
    // What `enums!` checks of a list of variants (tag, fields): a tag 0, without fields.
    assert!(tag_zero_ends(&[(1, 2), (0, 0)], false));
    assert!(!tag_zero_ends(&[(0, 1), (1, 0)], false), "tag 0 has fields");
    assert!(!tag_zero_ends(&[(1, 0), (2, 0)], true), "no tag 0");
    assert!(tag_zero_ends(&[(0, 1), (1, 0)], true), "listed `@fielded`");
}

/// An encoding starts with what it is, its version and the AST format of its fragments: bytes
/// of another are refused by every reader.
#[test]
fn bytes_of_another_format_are_refused() {
    let mut w = Writer::new();
    7u32.enc(&mut w);
    let (bytes, files) = w.finish();
    let arena = Arena::default();
    let reads = |bytes: &[u8]| {
        let mut r = Reader::new(bytes, &files);
        let whole = u32::dec(&mut r) == 7 && r.done();
        // SAFETY: nothing decoded outlives `arena`.
        let mut a = unsafe { Reader::with_arena(bytes, &files, &arena) };
        let with_arena = u32::dec(&mut a) == 7 && a.done();
        let (shared, shared_files) = (
            std::rc::Rc::new(bytes.to_vec()),
            std::rc::Rc::new(files.clone()),
        );
        // SAFETY: as above.
        let mut u = unsafe { Reader::over_unit(&shared, 0, &shared_files, &arena) };
        let over_unit = u32::dec(&mut u) == 7 && u.done();
        (whole, with_arena, over_unit)
    };
    assert_eq!(reads(&bytes), (true, true, true));
    // The encoding's own version, then the AST format's.
    for at in [
        MAGIC.len() - 1,
        MAGIC.len() + velt_syntax::AST_FORMAT.len() - 1,
    ] {
        let mut other = bytes.clone();
        other[at] ^= 1;
        assert_eq!(reads(&other), (false, false, false), "byte {at}");
    }
}

/// A fragment's span in another file than its node's goes through the unit's file table, as
/// every span does: in a session that numbers its files differently it is in that session's
/// file. So do the spans of a type a context owns.
#[test]
fn a_fragment_s_spans_in_another_file_are_the_session_s() {
    use velt_syntax::ast::{ItemKind, TypeAlias, TypeExpr};
    let (mut module, _) = velt_syntax::parse_file(FileId(3), "type A = { x: number };\n");
    let ItemKind::TypeAlias(alias) = &mut module.items[0].kind else {
        panic!("a type alias");
    };
    // The alias's type was written in another file (a merged declaration's part).
    alias.ty.span.file = FileId(9);
    let alias = &*alias;
    let mut w = Writer::new();
    (alias, alias.ty.clone()).enc(&mut w);
    let (bytes, files) = w.finish();
    assert_eq!(files, vec![FileId(3), FileId(9)]);
    let session: Vec<FileId> = files.iter().map(|f| FileId(f.0 + 100)).collect();
    let arena = Arena::default();
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::with_arena(&bytes, &session, &arena) };
    let (back, owned): (&TypeAlias, TypeExpr) = Codec::dec(&mut r);
    assert!(r.done());
    assert_eq!(back.name.span.file, FileId(103));
    assert_eq!(
        back.ty.span.file,
        FileId(109),
        "the other file, in this session"
    );
    assert_eq!(owned.span.file, FileId(109), "a type the context owns");
    // A file the table lacks is a failed decode.
    let mut r = unsafe { Reader::with_arena(&bytes, &session[..1], &arena) };
    let _: (&TypeAlias, TypeExpr) = Codec::dec(&mut r);
    assert!(!r.done());
}

/// References to a class and to an interface keep their methods' signatures, not their bodies.
#[test]
fn class_and_interface_references_keep_no_method_bodies() {
    use velt_syntax::ast::{InterfaceDecl, ItemKind, TypeDecl};
    let src = "class C {\n  constructor() { console.log(1); }\n  m(): number { return 1; }\n}\n\
               interface I {\n  d(): number { return 2; }\n  s(): void;\n}\n";
    let (module, diags) = velt_syntax::parse_file(FileId(3), src);
    assert!(diags.is_empty(), "{diags:?}");
    let (class, iface) = match (&module.items[0].kind, &module.items[1].kind) {
        (ItemKind::Class(c), ItemKind::Interface(i)) => (c, i),
        _ => panic!("a class and an interface"),
    };
    assert!(!class.methods[0].decl.body.stmts.is_empty());
    assert!(iface.methods[0]
        .body
        .as_ref()
        .is_some_and(|b| !b.stmts.is_empty()));
    let mut w = Writer::new();
    (class, iface).enc(&mut w);
    let (bytes, files) = w.finish();
    let arena = Arena::default();
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::with_arena(&bytes, &files, &arena) };
    let (c, i): (&TypeDecl, &InterfaceDecl) = Codec::dec(&mut r);
    assert!(r.done());
    assert_eq!(c.methods.len(), 1);
    assert!(c.methods[0].decl.body.stmts.is_empty(), "a method's body");
    assert!(
        c.constructor
            .as_ref()
            .is_some_and(|k| k.body.stmts.is_empty()),
        "the constructor's"
    );
    assert_eq!(
        format!("{:?}", c.methods[0].decl.sig),
        format!("{:?}", class.methods[0].decl.sig)
    );
    assert_eq!(i.methods.len(), 2);
    assert!(
        i.methods[0]
            .body
            .as_ref()
            .is_some_and(|b| b.stmts.is_empty()),
        "a default method's body"
    );
    assert!(i.methods[1].body.is_none());
}

/// A reference decoded as one type is not handed out as another, and an entry outside the
/// encoding is a failed reader, not a panic.
#[test]
fn a_node_of_one_type_is_not_another_s_and_a_bad_range_fails() {
    use velt_syntax::ast::{FnDecl, ItemKind, TypeAlias};
    let (module, _) = velt_syntax::parse_file(FileId(3), "function f(): number { return 1; }\n");
    let ItemKind::Function(f) = &module.items[0].kind else {
        panic!("a function");
    };
    let mut w = Writer::new();
    (f, f).enc(&mut w);
    let (bytes, files) = w.finish();
    let (bytes, files) = (std::rc::Rc::new(bytes), std::rc::Rc::new(files));
    let arena = Arena::default();
    // SAFETY: what is decoded is dropped in this function, before `arena`.
    let mut r = unsafe { Reader::over_unit(&bytes, 0, &files, &arena) };
    // The node is decoded as a function first; the second reference asks for a type alias.
    let (_, wrong): (&FnDecl, &TypeAlias) = Codec::dec(&mut r);
    assert!(r.failed());
    assert!(!wrong.name.name.is_empty(), "a node stands in");
    let src = r.src.clone().expect("a unit's reader");
    #[allow(clippy::reversed_empty_ranges)]
    for range in [
        bytes.len()..bytes.len() + 1,
        usize::MAX - 1..usize::MAX,
        5..2,
    ] {
        assert!(src.reader(range).failed());
    }
    assert!(!src.reader(0..bytes.len()).failed());
}
