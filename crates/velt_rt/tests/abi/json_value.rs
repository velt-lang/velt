//! `json.Value` through the C ABI: the golden program's calls, accessors, handle lifetimes,
//! stringify round trips, errors and deep nesting.

use super::json_reader::borrow;
use crate::json::value_abi::*;
use crate::str::{velt_rt_str_drop, VeltStr};
use crate::strbuf::*;
use std::mem::MaybeUninit;

fn owned_text(mut s: VeltStr) -> String {
    let text = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).unwrap();
    unsafe { velt_rt_str_drop(&mut s) };
    text
}

fn parse(src: &str) -> Result<ValueHandle, String> {
    let mut h = MaybeUninit::uninit();
    let mut err = MaybeUninit::uninit();
    unsafe {
        if velt_rt_json_parse_value(&borrow(src), h.as_mut_ptr(), err.as_mut_ptr()) == 1 {
            Ok(h.assume_init())
        } else {
            assert!(h.assume_init().is_null());
            Err(owned_text(err.assume_init()))
        }
    }
}

fn stringify(h: ValueHandle) -> String {
    let mut out = MaybeUninit::uninit();
    unsafe { velt_rt_json_value_stringify(h, out.as_mut_ptr()) };
    owned_text(unsafe { out.assume_init() })
}

fn get(h: ValueHandle, key: &str) -> ValueHandle {
    unsafe { velt_rt_json_value_get(h, &borrow(key)) }
}

fn free(h: ValueHandle) {
    unsafe { velt_rt_json_value_free(h) };
}

#[test]
fn golden_program() {
    let v = parse(r#"{"a":[1,2.5,{"b":null}],"c":true,"s":"x\ny"}"#).unwrap();
    // v.get("a")?.at(2)?.get("b")?.isNull() ?? false
    let a = get(v, "a");
    let a2 = unsafe { velt_rt_json_value_at(a, 2) };
    let b = get(a2, "b");
    assert_eq!(unsafe { velt_rt_json_value_kind(b) }, KIND_NULL);
    assert_eq!(
        stringify(v),
        r#"{"a":[1,2.5,{"b":null}],"c":true,"s":"x\ny"}"#
    );
    for h in [b, a2, a, v] {
        free(h);
    }
}

#[test]
fn accessors_and_handle_lifetimes() {
    let v = parse(r#" {"n": -2.5e1, "t": true, "f": false, "s": "h\u00e9", "arr": [1, "x"], "o": {"k": []}} "#).unwrap();
    let child = |k| get(v, k);
    let (n, t, f, s, arr, o) = (
        child("n"),
        child("t"),
        child("f"),
        child("s"),
        child("arr"),
        child("o"),
    );
    assert!(get(v, "missing").is_null() && get(n, "x").is_null());
    // The root can go first: sub-handles keep their nodes alive.
    free(v);
    unsafe {
        let kinds = [n, t, f, s, arr, o].map(|h| velt_rt_json_value_kind(h));
        assert_eq!(
            kinds,
            [
                KIND_NUMBER,
                KIND_BOOL,
                KIND_BOOL,
                KIND_STRING,
                KIND_ARRAY,
                KIND_OBJECT
            ]
        );
        assert_eq!(velt_rt_json_value_kind(ValueHandle::NULL), KIND_NONE);
        assert_eq!(velt_rt_json_value_as_f64(n), -25.0);
        assert!(velt_rt_json_value_as_f64(s).is_nan());
        assert_eq!(
            (velt_rt_json_value_as_bool(t), velt_rt_json_value_as_bool(f)),
            (1, 0)
        );
        assert_eq!(velt_rt_json_value_as_bool(n), 0);
        let mut out = MaybeUninit::uninit();
        assert_eq!(velt_rt_json_value_as_str(s, out.as_mut_ptr()), 1);
        assert_eq!(owned_text(out.assume_init_read()), "hé");
        assert_eq!(velt_rt_json_value_as_str(n, out.as_mut_ptr()), 0);
        let lens = [arr, o, s, n, ValueHandle::NULL].map(|h| velt_rt_json_value_len(h));
        assert_eq!(lens, [2, 1, 3, 0, 0]);
        let x = velt_rt_json_value_at(arr, 1);
        assert_eq!(stringify(x), "\"x\"");
        assert!(velt_rt_json_value_at(arr, 2).is_null());
        let k = velt_rt_json_value_at(o, 0);
        assert_eq!(stringify(k), "[]");
        assert_eq!(velt_rt_json_value_key_at(o, 0, out.as_mut_ptr()), 1);
        assert_eq!(owned_text(out.assume_init_read()), "k");
        assert_eq!(velt_rt_json_value_key_at(o, 1, out.as_mut_ptr()), 0);
        assert_eq!(velt_rt_json_value_key_at(arr, 0, out.as_mut_ptr()), 0);
        let c = velt_rt_json_value_clone(o);
        free(o);
        assert_eq!(stringify(c), r#"{"k":[]}"#);
        assert!(velt_rt_json_value_clone(ValueHandle::NULL).is_null());
        for h in [n, t, f, s, arr, x, k, c, ValueHandle::NULL] {
            free(h);
        }
    }
    assert_eq!(stringify(ValueHandle::NULL), "null");
}

#[test]
fn stringify_round_trips() {
    // Expected values: JSON.stringify(JSON.parse(doc)) in node.
    let cases = [
        (
            " [ 1.0 , 1e2, -0, 1E-7, 0.000001, 1e21, 123456789012345678901234567890 ] ",
            "[1,100,0,1e-7,0.000001,1e+21,1.2345678901234568e+29]",
        ),
        (
            r#""\u00e9\u001F\/\"\\\b\f\n\r\t\u2028\ud83d\ude00""#,
            "\"é\\u001f/\\\"\\\\\\b\\f\\n\\r\\t\u{2028}😀\"",
        ),
        (r#"{"a":1,"b":2,"a":3}"#, r#"{"a":3,"b":2}"#),
        (
            r#"{"z":{},"y":[],"x":[{}],"":""}"#,
            r#"{"z":{},"y":[],"x":[{}],"":""}"#,
        ),
        ("null", "null"),
        ("true", "true"),
        ("\"\"", "\"\""),
    ];
    for (doc, want) in cases {
        let h = parse(doc).unwrap();
        assert_eq!(stringify(h), want, "{doc}");
        free(h);
    }
}

#[test]
fn large_objects_use_the_index() {
    let mut doc = String::from("{");
    for i in 0..100 {
        doc += &format!("\"k{i}\":{i},");
    }
    doc += "\"k5\":\"dup\",\"k99\":null}";
    let v = parse(&doc).unwrap();
    assert_eq!(unsafe { velt_rt_json_value_len(v) }, 100);
    let k5 = get(v, "k5");
    let k42 = get(v, "k42");
    assert_eq!(
        (stringify(k5), stringify(k42)),
        ("\"dup\"".to_string(), "42".to_string())
    );
    assert!(stringify(v).starts_with(r#"{"k0":0,"k1":1,"k2":2,"k3":3,"k4":4,"k5":"dup","k6":6"#));
    assert!(get(v, "k100").is_null());
    for h in [k5, k42, v] {
        free(h);
    }
}

#[test]
fn syntax_errors() {
    let cases = [
        ("[1,]", "invalid JSON: unexpected character ']' (byte 3)"),
        ("", "invalid JSON: unexpected end of input (byte 0)"),
        (
            "{} x",
            "invalid JSON: unexpected trailing characters (byte 3)",
        ),
        (r#"{"a" 1}"#, "invalid JSON: expected ':' (byte 5)"),
        ("[1 2]", "invalid JSON: expected ',' or ']' (byte 3)"),
        ("{\"a\":1,}", "invalid JSON: expected string key (byte 7)"),
        ("[\"\\x\"]", "invalid JSON: invalid escape (byte 3)"),
        ("[01]", "invalid JSON: expected ',' or ']' (byte 2)"),
        ("[1.]", "invalid JSON: invalid number (byte 3)"),
        ("[.5]", "invalid JSON: unexpected character '.' (byte 1)"),
        ("[+1]", "invalid JSON: unexpected character '+' (byte 1)"),
        ("[nul]", "invalid JSON: unexpected character ']' (byte 4)"),
        ("[NaN]", "invalid JSON: unexpected character 'N' (byte 1)"),
        ("\"abc", "invalid JSON: unexpected end of input (byte 4)"),
        (
            "[\"\t\"]",
            "invalid JSON: control character in string (byte 2)",
        ),
    ];
    for (doc, want) in cases {
        assert_eq!(parse(doc).unwrap_err(), want, "{doc}");
    }
}

#[test]
fn deep_nesting() {
    let depth = 1_000_000;
    let doc = "[".repeat(depth) + &"]".repeat(depth);
    let v = parse(&doc).unwrap();
    assert_eq!(stringify(v), doc);
    // Keep a handle deep inside, free the root (iterative teardown), then free the inner one.
    let mut inner = unsafe { velt_rt_json_value_clone(v) };
    for _ in 0..depth / 2 {
        let next = unsafe { velt_rt_json_value_at(inner, 0) };
        free(inner);
        inner = next;
    }
    free(v);
    assert_eq!(stringify(inner).len(), depth);
    free(inner);
    let objects = "{\"a\":".repeat(depth) + "1" + &"}".repeat(depth);
    let v = parse(&objects).unwrap();
    assert_eq!(stringify(v), objects);
    free(v);
    assert!(parse(&"[".repeat(depth)).is_err());
}

#[test]
fn builder_embeds_values() {
    let v = parse(r#"{"x":[1,"two"]}"#).unwrap();
    let mut b = MaybeUninit::uninit();
    let mut out = MaybeUninit::uninit();
    unsafe {
        velt_rt_strbuf_new(0, b.as_mut_ptr());
        let mut b = b.assume_init();
        velt_rt_strbuf_push_bytes(&mut b, b"{\"v\":".as_ptr(), 5);
        velt_rt_strbuf_push_json_value(&mut b, v.ptr());
        velt_rt_strbuf_push_byte(&mut b, b'}');
        velt_rt_strbuf_finish(&mut b, out.as_mut_ptr());
    }
    assert_eq!(
        owned_text(unsafe { out.assume_init() }),
        r#"{"v":{"x":[1,"two"]}}"#
    );
    free(v);
}

#[test]
fn reader_reads_values_as_trees() {
    use crate::json::reader_abi::*;
    // A typed decoder's `JsonValue` field: any value, never the class's `handle` field.
    let src = r#"{"a":[1,{"handle":4096}],"b":"s","c":[1,}"#;
    unsafe {
        let r = velt_rt_json_reader_new(&borrow(src));
        assert_eq!(velt_rt_json_reader_expect_object_start(r), 1);
        let mut key = MaybeUninit::uninit();
        let mut values = Vec::new();
        while velt_rt_json_reader_next_key(r, key.as_mut_ptr()) == 1 {
            let mut h = MaybeUninit::uninit();
            if velt_rt_json_reader_read_value(r, h.as_mut_ptr()) == 0 {
                break;
            }
            let h = h.assume_init();
            values.push(stringify(h));
            free(h);
        }
        assert_eq!(values, [r#"[1,{"handle":4096}]"#, r#""s""#]);
        let mut msg = MaybeUninit::uninit();
        velt_rt_json_error(r, &borrow("value"), &borrow("$.c"), msg.as_mut_ptr());
        assert_eq!(
            owned_text(msg.assume_init()),
            "invalid JSON at $.c: unexpected character '}' (byte 40)"
        );
        velt_rt_json_reader_free(r);
    }
}
