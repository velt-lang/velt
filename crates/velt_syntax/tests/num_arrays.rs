//! A large array literal of number literals parses as one packed node (`ExprKind::NumArray`,
//! #2421) that stands for exactly the `Array` the parser makes without packing.

mod common;

use common::*;
use velt_syntax::parse_file_unpacked;

/// `[e0, e1, …]` of `n` elements, `f(i)` the text of element `i`.
fn array(n: usize, f: impl Fn(usize) -> String) -> String {
    let elems: Vec<String> = (0..n).map(f).collect();
    format!("[{}]", elems.join(", "))
}

/// The initializer of the first item of `src` (`const t = <init>;`), parsed as the compiler does.
fn init(src: &str) -> Expr {
    let m = parse_ok(src);
    match &m.items[0].kind {
        ItemKind::Var(v) => v.init.clone().expect("an initializer"),
        k => panic!("expected a variable, got {k:?}"),
    }
}

fn packed(src: &str) -> Option<NumArray> {
    match init(src).kind {
        ExprKind::NumArray(n) => Some(*n),
        _ => None,
    }
}

/// The module `src` parses to, packed or not, unpacked again, is the module the parser makes
/// without packing: same nodes, ids and spans.
fn same_as_unpacked(src: &str) {
    let (m, d) = parse_file(FileId(0), src);
    assert!(d.is_empty(), "{d:?}");
    let (plain, _) = parse_file_unpacked(FileId(0), src);
    let unpack = |m: &Module| -> Vec<String> {
        m.items
            .iter()
            .map(|i| match &i.kind {
                ItemKind::Var(v) => {
                    let e = v.init.as_ref().expect("an initializer");
                    format!("{:?}", e.unpacked())
                }
                k => format!("{k:?}"),
            })
            .collect()
    };
    assert_eq!(unpack(&m), unpack(&plain), "for {src}");
}

#[test]
fn sixteen_number_literals_are_packed_fifteen_are_not() {
    let src = |n| format!("const t = {};", array(n, |i| i.to_string()));
    assert!(packed(&src(15)).is_none());
    let n = packed(&src(16)).expect("packed");
    assert_eq!(n.elems.len(), 16);
    assert_eq!(n.elems[3].bits, 3);
    assert!(!n.elems[3].float && !n.elems[3].negative());
    let big = packed(&src(2000)).expect("packed");
    assert_eq!(big.elems.len(), 2000);
}

#[test]
fn one_element_that_is_not_a_plain_number_literal_keeps_an_array() {
    let with = |odd: &str| {
        let elems = array(20, |i| match i {
            7 => odd.to_string(),
            _ => i.to_string(),
        });
        format!("const t = {elems};")
    };
    for odd in [
        "x",
        "1 + 2",
        "(3)",
        "-x",
        "+1",
        "--x",
        "1u8",
        "2.5f32",
        "\"s\"",
        "true",
        "null",
        "...xs",
        "0x1_0000_0000_0000_0000",
        "- -1",
        "f(1)",
        "1 as u8",
        "[1]",
    ] {
        assert!(packed(&with(odd)).is_none(), "{odd}");
        same_as_unpacked(&with(odd));
    }
    // A hole.
    assert!(
        packed("const t = [1, 2, , 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];").is_none()
    );
}

#[test]
fn negated_hex_and_float_literals_are_packed() {
    let src = format!(
        "const t = {};",
        array(18, |i| match i % 6 {
            0 => format!("-{i}"),
            1 => format!("0x{i:x}"),
            2 => format!("{i}.5"),
            3 => format!("- 0x{i:X}"),
            4 => format!("-{i}e-3"),
            _ => format!("0b{i:b}"),
        })
    );
    let n = packed(&src).expect("packed");
    let e = &n.elems;
    assert!(e[0].negative() && !e[0].float && e[0].bits == 0);
    assert!(!e[1].negative() && e[1].bits == 1);
    assert!(e[2].float && f64::from_bits(e[2].bits) == 2.5);
    assert!(e[3].negative() && e[3].bits == 3 && e[3].lit_lo == e[3].lo + 2);
    assert!(e[4].negative() && e[4].float && f64::from_bits(e[4].bits) == 4e-3);
    assert_eq!(&src[e[3].lo as usize..e[3].hi as usize], "- 0x3");
    assert_eq!(&src[e[3].lit_lo as usize..e[3].hi as usize], "0x3");
    same_as_unpacked(&src);
}

#[test]
fn a_trailing_comma_and_the_largest_u64_are_packed() {
    let mut elems: Vec<String> = (0..16).map(|i| i.to_string()).collect();
    elems[5] = "18446744073709551615".into();
    let src = format!("const t = [\n  {},\n];", elems.join(",\n  "));
    let n = packed(&src).expect("packed");
    assert_eq!(n.elems[5].bits, u64::MAX);
    same_as_unpacked(&src);
}

#[test]
fn spreads_are_not_packed() {
    let src = format!(
        "const t = [...xs, {}];",
        array(20, |i| i.to_string()).trim_matches(['[', ']'])
    );
    assert!(packed(&src).is_none());
}

#[test]
fn the_nodes_after_a_packed_array_keep_their_ids() {
    // Ids: the array reserves its elements' ids, so the next node's id is the same as without
    // packing (sema keys tables by node id).
    let src = format!(
        "const t = {};\nconst u = [y, -1];\nconst v = {};",
        array(40, |i| if i % 3 == 0 {
            format!("-{i}")
        } else {
            i.to_string()
        }),
        array(16, |i| format!("{i}.25")),
    );
    same_as_unpacked(&src);
    let (m, _) = parse_file(FileId(0), &src);
    let (plain, _) = parse_file_unpacked(FileId(0), &src);
    assert_eq!(format!("{:?}", m.items[1]), format!("{:?}", plain.items[1]));
}

#[test]
fn a_packed_array_is_an_array_to_the_expression_printer() {
    let src = format!("const t = {};", array(16, |i| format!("-{i}")));
    let e = init(&src);
    assert!(matches!(e.kind, ExprKind::NumArray(_)));
    assert_eq!(
        sx(&e),
        format!(
            "[{}]",
            (0..16)
                .map(|i| format!("(- {i})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    );
}

#[test]
fn a_packed_array_round_trips_through_the_ast_encoding() {
    let src = format!("const t = {};", array(30, |i| format!("-{i}.5")));
    let (m, _) = parse_file(FileId(3), &src);
    let bytes = velt_syntax::encode_module(&m, FileId(3));
    let back = velt_syntax::decode_module(&bytes, FileId(3)).expect("decodes");
    assert_eq!(format!("{m:?}"), format!("{back:?}"));
}
