//! Parameter parsing and row encoding edge cases.

use super::*;
use std::borrow::Cow;

fn named<'a>(pairs: &[(&'a str, DbValue<'a>)]) -> Params<'a> {
    Params::Named(
        pairs
            .iter()
            .map(|(k, v)| (Cow::Borrowed(*k), v.clone()))
            .collect(),
    )
}

#[test]
fn parses_named_positional_scalar_and_empty() {
    assert_eq!(parse_params(b"").unwrap(), Params::None);
    assert_eq!(parse_params(b"  ").unwrap(), Params::None);
    assert_eq!(
        parse_params(br#"{"id":1,"name":"a","ok":true,"x":null}"#).unwrap(),
        named(&[
            ("id", DbValue::Int(1)),
            ("name", DbValue::Text("a".into())),
            ("ok", DbValue::Bool(true)),
            ("x", DbValue::Null),
        ])
    );
    assert_eq!(
        parse_params(br#"[1, "b", false, 2.5]"#).unwrap(),
        Params::Positional(vec![
            DbValue::Int(1),
            DbValue::Text("b".into()),
            DbValue::Bool(false),
            DbValue::Float(2.5),
        ])
    );
    assert_eq!(
        parse_params(b"42").unwrap(),
        Params::Positional(vec![DbValue::Int(42)])
    );
    assert_eq!(parse_params(b"{}").unwrap(), Params::Named(vec![]));
    assert_eq!(parse_params(b"[]").unwrap(), Params::Positional(vec![]));
}

#[test]
fn integers_are_exact_and_floats_stay_floats() {
    let p = parse_params(b"[9007199254740993,-9223372036854775808,9223372036854775808,1e2,1.0,-0]")
        .unwrap();
    assert_eq!(
        p,
        Params::Positional(vec![
            DbValue::Int(9007199254740993),
            DbValue::Int(i64::MIN),
            DbValue::Float(9223372036854775808.0),
            DbValue::Float(100.0),
            DbValue::Float(1.0),
            DbValue::Int(0),
        ])
    );
}

#[test]
fn strings_borrow_unless_escaped() {
    let Params::Positional(v) = parse_params(r#"["plain","a\"b\u00e9\n"]"#.as_bytes()).unwrap()
    else {
        panic!("positional expected");
    };
    assert!(matches!(&v[0], DbValue::Text(Cow::Borrowed("plain"))));
    assert_eq!(v[1], DbValue::Text(Cow::Owned("a\"b\u{e9}\n".to_string())));
}

#[test]
fn byte_arrays_are_blobs() {
    assert_eq!(
        parse_params(br#"{"data":[0,127,255],"empty":[]}"#).unwrap(),
        named(&[
            ("data", DbValue::Bytes(vec![0, 127, 255])),
            ("empty", DbValue::Bytes(vec![])),
        ])
    );
    for bad in [
        &br#"{"d":[256]}"#[..],
        br#"{"d":[-1]}"#,
        br#"{"d":[1.5]}"#,
        br#"{"d":["a"]}"#,
    ] {
        let e = parse_params(bad).unwrap_err();
        assert!(e.contains("parameter \"d\"") && e.contains("0-255"), "{e}");
    }
}

#[test]
fn rejects_nested_objects_and_bad_json() {
    let e = parse_params(br#"{"a":{"b":1}}"#).unwrap_err();
    assert!(e.contains("nested objects"), "{e}");
    let e = parse_params(br#"[1,{}]"#).unwrap_err();
    assert!(e.starts_with("parameter 2:"), "{e}");
    for bad in [
        &b"{"[..],
        b"[1,",
        b"[1 2]",
        b"{\"a\" 1}",
        b"nul",
        b"1 2",
        b"{1:2}",
    ] {
        assert!(
            parse_params(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn named_lookup_takes_the_last_duplicate() {
    let p = parse_params(br#"{"a":1,"b":2,"a":3}"#).unwrap();
    assert_eq!(p.named("a"), Some(&DbValue::Int(3)));
    assert_eq!(p.named("b"), Some(&DbValue::Int(2)));
    assert_eq!(p.named("c"), None);
    assert_eq!(Params::None.named("a"), None);
}

fn two_rows() -> RowWriter {
    let mut w = RowWriter::new(["id", "we\"ird"]);
    w.begin_row();
    w.int(i64::MAX);
    w.text(b"tab\there");
    w.end_row();
    w.begin_row();
    w.null();
    w.bytes(&[0, 1, 255]);
    w.end_row();
    w
}

#[test]
fn rows_encode_as_an_array_of_objects() {
    let w = two_rows();
    assert_eq!((w.columns(), w.rows()), (2, 2));
    assert_eq!(
        String::from_utf8(w.into_array()).unwrap(),
        r#"[{"id":9223372036854775807,"we\"ird":"tab\there"},{"id":null,"we\"ird":[0,1,255]}]"#
    );
    let empty = RowWriter::new(["a"]);
    assert_eq!(empty.into_array(), b"[]");
}

#[test]
fn single_row_and_no_row() {
    let mut w = RowWriter::new(["x"]);
    assert!(w.into_rows().is_empty());
    w = RowWriter::new(["x"]);
    w.begin_row();
    w.bool(true);
    w.end_row();
    assert_eq!(w.into_rows(), br#"{"x":true}"#);
}

#[test]
fn floats_are_shortest_round_trip_and_non_finite_is_null() {
    let mut w = RowWriter::new(["a", "b", "c", "d", "e", "f", "g", "h"]);
    w.begin_row();
    for v in [
        0.1 + 0.2,
        3.0,
        -0.0,
        1e21,
        1.5e-7,
        f64::NAN,
        f64::NEG_INFINITY,
        9007199254740992.0,
    ] {
        w.float(v);
    }
    w.end_row();
    assert_eq!(
        String::from_utf8(w.into_rows()).unwrap(),
        r#"{"a":0.30000000000000004,"b":3,"c":0,"d":1e21,"e":1.5e-7,"f":null,"g":null,"h":9007199254740992.0}"#
    );
}

#[test]
fn text_is_escaped_and_invalid_utf8_replaced() {
    let mut w = RowWriter::new(["t", "u"]);
    w.begin_row();
    w.text("é😀\u{1}\\".as_bytes());
    w.text(b"a\xffb");
    w.end_row();
    assert_eq!(
        String::from_utf8(w.into_rows()).unwrap(),
        "{\"t\":\"é😀\\u0001\\\\\",\"u\":\"a\u{fffd}b\"}"
    );
}
