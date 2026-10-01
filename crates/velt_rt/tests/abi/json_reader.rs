//! The JSON pull reader driven the way a generated `JSON.parse<User>` decoder would drive it
//! (tests/golden/m4/json.vlt), plus token-level checks on valid and invalid documents.

use crate::json::reader::*;
use crate::json::reader_abi::*;
use crate::str::{velt_rt_str_drop, VeltStr};
use std::mem::MaybeUninit;

fn lit(s: &'static str) -> VeltStr {
    VeltStr::from_static(s.as_bytes())
}

/// A borrowed `VeltStr` view of `s` (static form: never freed); valid while `s` lives.
pub fn borrow(s: &str) -> VeltStr {
    unsafe { VeltStr::borrowed(s.as_ptr(), s.len()) }
}

fn owned_text(mut s: VeltStr) -> String {
    let text = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).unwrap();
    unsafe { velt_rt_str_drop(&mut s) };
    text
}

/// `struct User { name: string; age: i64; tags: string[]; email?: string; }`
#[derive(Debug, PartialEq)]
pub struct User {
    pub name: String,
    pub age: i64,
    pub tags: Vec<String>,
    pub email: Option<String>,
}

/// Fail with the runtime-built message (what generated code throws as `JsonError`).
unsafe fn fail(r: *mut Reader, expected: &str, path: &str) -> String {
    let (e, p) = (borrow(expected), borrow(path));
    let mut out = MaybeUninit::uninit();
    velt_rt_json_error(r, &e, &p, out.as_mut_ptr());
    owned_text(out.assume_init())
}

unsafe fn read_string(r: *mut Reader, path: &str) -> Result<String, String> {
    let mut s = MaybeUninit::uninit();
    if velt_rt_json_reader_read_string(r, s.as_mut_ptr()) == 0 {
        return Err(fail(r, "string", path));
    }
    Ok(owned_text(s.assume_init()))
}

unsafe fn read_tags(r: *mut Reader) -> Result<Vec<String>, String> {
    if velt_rt_json_reader_expect_array_start(r) == 0 {
        return Err(fail(r, "array", "$.tags"));
    }
    let mut tags = Vec::new();
    loop {
        match velt_rt_json_reader_array_next(r) {
            STEP_END => return Ok(tags),
            STEP_MORE => tags.push(read_string(r, &format!("$.tags[{}]", tags.len()))?),
            _ => return Err(fail(r, "array", "$.tags")),
        }
    }
}

/// One member of `User`; unknown keys are skipped.
unsafe fn read_member(
    r: *mut Reader,
    key: &[u8],
    u: &mut User,
    seen: &mut [bool; 3],
) -> Result<(), String> {
    match key {
        b"name" => (u.name, seen[0]) = (read_string(r, "$.name")?, true),
        b"age" => {
            if velt_rt_json_reader_read_i64(r, &mut u.age) == 0 {
                return Err(fail(r, "i64", "$.age"));
            }
            seen[1] = true;
        }
        b"tags" => (u.tags, seen[2]) = (read_tags(r)?, true),
        b"email" => {
            u.email = if velt_rt_json_reader_peek(r) == TOKEN_NULL {
                velt_rt_json_reader_read_null(r);
                None
            } else {
                Some(read_string(r, "$.email")?)
            }
        }
        _ => {
            if velt_rt_json_reader_skip_value(r) == 0 {
                let path = format!("$.{}", String::from_utf8_lossy(key));
                return Err(fail(r, "value", &path));
            }
        }
    }
    Ok(())
}

/// Decode one `User` object at the reader's position (error paths assume it is the root).
pub unsafe fn decode_user_object(r: *mut Reader) -> Result<User, String> {
    if velt_rt_json_reader_expect_object_start(r) == 0 {
        return Err(fail(r, "object", "$"));
    }
    let mut u = User {
        name: String::new(),
        age: 0,
        tags: Vec::new(),
        email: None,
    };
    let mut seen = [false; 3];
    loop {
        let mut key = MaybeUninit::<VeltStr>::uninit();
        match velt_rt_json_reader_next_key(r, key.as_mut_ptr()) {
            STEP_END => break,
            STEP_MORE => {
                let mut key = key.assume_init();
                let res = read_member(r, key.as_bytes(), &mut u, &mut seen);
                velt_rt_str_drop(&mut key);
                res?;
            }
            _ => return Err(fail(r, "object", "$")),
        }
    }
    for (i, name) in ["name", "age", "tags"].iter().enumerate() {
        if !seen[i] {
            return Err(fail(r, &format!("field \"{name}\""), "$"));
        }
    }
    Ok(u)
}

unsafe fn decode_user_with(r: *mut Reader) -> Result<User, String> {
    let u = decode_user_object(r)?;
    if velt_rt_json_reader_end(r) == 0 {
        return Err(fail(r, "end of input", "$"));
    }
    Ok(u)
}

/// `JSON.parse<User>(src)`.
pub fn decode_user(src: &str) -> Result<User, String> {
    let s = borrow(src);
    unsafe {
        let r = velt_rt_json_reader_new(&s);
        let res = decode_user_with(r);
        velt_rt_json_reader_free(r);
        res
    }
}

#[test]
fn golden_documents() {
    let back = decode_user(r#"{"name":"bob","age":41,"tags":[],"email":"b@x.io"}"#).unwrap();
    assert_eq!(
        back,
        User {
            name: "bob".into(),
            age: 41,
            tags: vec![],
            email: Some("b@x.io".into())
        }
    );
    let err = decode_user(r#"{"name": 1, "age": 2, "tags": []}"#).unwrap_err();
    assert_eq!(err, "expected string at $.name");
}

#[test]
fn escapes_unknown_keys_and_duplicates() {
    // `~` stands for the two characters backslash-u (a JSON \u escape).
    let doc = r#" { "x":{"deep":[1,{"a":null},"s\"",true,false,-1.5e3]}, "na~006de" : "a\"b\\c\/d\b\f\n\r\t~00e9~d83d~de00~d800x~dc00~20AC",
        "age":1e2, "tags":["", "é"], "email":null, "age": 7 } "#
        .replace('~', "\\u");
    let u = decode_user(&doc).unwrap();
    assert_eq!(u.name, "a\"b\\c/d\u{8}\u{c}\n\r\té😀\u{FFFD}x\u{FFFD}€");
    assert_eq!(
        (u.age, u.tags, u.email),
        (7, vec![String::new(), "é".to_string()], None)
    );
}

#[test]
fn error_messages() {
    let cases: &[(&str, &str)] = &[
        (
            r#"{"name":"x","age":1.5,"tags":[]}"#,
            "expected i64 at $.age",
        ),
        (
            r#"{"name":"x","age":1,"tags":["a",2]}"#,
            "expected string at $.tags[1]",
        ),
        (
            r#"{"name":"x","age":1,"tags":{}}"#,
            "expected array at $.tags",
        ),
        (r#"[1,2]"#, "expected object at $"),
        (r#"{"name":"x","tags":[]}"#, "expected field \"age\" at $"),
        (
            r#"{"name":"x","age":9223372036854775808,"tags":[]}"#,
            "expected i64 at $.age",
        ),
        (r#"{"name":"x"}"#, "expected field \"age\" at $"),
        (
            r#"{"name":"x""#,
            "invalid JSON at $: unexpected end of input (byte 11)",
        ),
        (
            r#"{"name":"x",}"#,
            "invalid JSON at $: expected string key (byte 12)",
        ),
        (
            r#"{"name":"x" "age":1}"#,
            "invalid JSON at $: expected ',' or '}' (byte 12)",
        ),
        (
            r#"{"name" "x"}"#,
            "invalid JSON at $: expected ':' (byte 8)",
        ),
        (
            r#"{"name":"x\q"}"#,
            "invalid JSON at $.name: invalid escape (byte 11)",
        ),
        (
            r#"{"name":"x\u12"}"#,
            "invalid JSON at $.name: invalid \\u escape (byte 14)",
        ),
        (
            "{\"name\":\"a\nb\"}",
            "invalid JSON at $.name: control character in string (byte 10)",
        ),
        (
            r#"{"name":"x","age":01,"tags":[]}"#,
            "invalid JSON at $: expected ',' or '}' (byte 19)",
        ),
        (
            r#"{"name":"x","age":-,"tags":[]}"#,
            "invalid JSON at $.age: invalid number (byte 19)",
        ),
        (
            r#"{"name":"x","age":1,"tags":["a" "b"]}"#,
            "invalid JSON at $.tags: expected ',' or ']' (byte 32)",
        ),
        (
            r#"{"name":"x","age":1,"tags":[],"z":[1,{"a":]}]}"#,
            "invalid JSON at $.z: unexpected character ']' (byte 42)",
        ),
        (
            r#"{"name":"x","age":1,"tags":[],"z":tru}"#,
            "invalid JSON at $.z: unexpected character '}' (byte 37)",
        ),
        (
            r#"{"name":"x","age":1,"tags":[]} x"#,
            "invalid JSON at $: unexpected trailing characters (byte 31)",
        ),
        (
            r#"{"name":"x","age":1,"tags":[]}{}"#,
            "invalid JSON at $: unexpected trailing characters (byte 30)",
        ),
        ("", "invalid JSON at $: unexpected end of input (byte 0)"),
        (
            "  é",
            "invalid JSON at $: unexpected character 'é' (byte 2)",
        ),
        (
            "\u{1}",
            "invalid JSON at $: unexpected character U+0001 (byte 0)",
        ),
        (
            r#"{"name":"x","age":1,"tags":[],"z":[[[[["#,
            "invalid JSON at $.z: unexpected end of input (byte 39)",
        ),
    ];
    for (doc, want) in cases {
        assert_eq!(decode_user(doc).unwrap_err(), *want, "{doc}");
    }
}

#[test]
fn integers_and_numbers() {
    let age =
        |v: &str| decode_user(&format!(r#"{{"name":"","tags":[],"age":{v}}}"#)).map(|u| u.age);
    assert_eq!(age("9223372036854775807"), Ok(i64::MAX));
    assert_eq!(age("-9223372036854775808"), Ok(i64::MIN));
    assert_eq!(age("-0"), Ok(0));
    assert_eq!(age("2.0"), Ok(2));
    assert_eq!(age("25e-1").unwrap_err(), "expected i64 at $.age");
    assert_eq!(age("1e400").unwrap_err(), "expected i64 at $.age");
    assert_eq!(age("\"1\"").unwrap_err(), "expected i64 at $.age");
    // read_f64: correctly rounded like JSON.parse.
    for (text, want) in [
        ("-0", -0.0f64),
        ("0.1", 0.1),
        ("1e-7", 1e-7),
        ("1.7976931348623157e308", f64::MAX),
        ("5e-324", 5e-324),
        ("123456789012345678901234567890", 1.2345678901234568e29),
        ("1E+2", 100.0),
        ("1e400", f64::INFINITY),
    ] {
        let src = borrow(text);
        let mut v = 0.0;
        unsafe {
            let r = velt_rt_json_reader_new(&src);
            assert_eq!(velt_rt_json_reader_peek(r), TOKEN_NUMBER);
            assert_eq!(velt_rt_json_reader_read_f64(r, &mut v), 1);
            assert_eq!(velt_rt_json_reader_end(r), 1);
            velt_rt_json_reader_free(r);
        }
        assert_eq!(v.to_bits(), want.to_bits(), "{text}");
    }
}

#[test]
fn peek_kinds_and_scalars() {
    let src = lit(r#" [null, true ,false,-1,"s",[],{}] "#);
    unsafe {
        let r = velt_rt_json_reader_new(&src);
        assert_eq!(velt_rt_json_reader_peek(r), TOKEN_ARRAY_START);
        assert_eq!(velt_rt_json_reader_expect_array_start(r), 1);
        let mut kinds = Vec::new();
        while velt_rt_json_reader_array_next(r) == STEP_MORE {
            let k = velt_rt_json_reader_peek(r);
            kinds.push(k);
            let mut b = 9u8;
            let ok = match k {
                TOKEN_TRUE | TOKEN_FALSE => {
                    let ok = velt_rt_json_reader_read_bool(r, &mut b);
                    assert_eq!(b, (k == TOKEN_TRUE) as u8);
                    ok
                }
                TOKEN_NULL => velt_rt_json_reader_read_null(r),
                _ => velt_rt_json_reader_skip_value(r),
            };
            assert_eq!(ok, 1);
        }
        assert_eq!(kinds, [1, 2, 3, 4, 5, 6, 8]);
        assert_eq!(velt_rt_json_reader_peek(r), TOKEN_EOF);
        assert_eq!(velt_rt_json_reader_end(r), 1);
        velt_rt_json_reader_free(r);
    }
    // After an error, everything fails and peek reports it.
    let src = lit("[1 2]");
    unsafe {
        let r = velt_rt_json_reader_new(&src);
        assert_eq!(velt_rt_json_reader_expect_array_start(r), 1);
        assert_eq!(velt_rt_json_reader_array_next(r), STEP_MORE);
        assert_eq!(velt_rt_json_reader_skip_value(r), 1);
        assert_eq!(velt_rt_json_reader_array_next(r), STEP_ERROR);
        assert_eq!(velt_rt_json_reader_peek(r), TOKEN_ERROR);
        assert_eq!(velt_rt_json_reader_skip_value(r), 0);
        velt_rt_json_reader_free(r);
    }
}

#[test]
fn keys_borrow_unless_escaped() {
    let doc = r#"{"plain":1,"esc~0041":2,"":3}"#.replace('~', "\\u");
    let src = borrow(&doc);
    unsafe {
        let r = velt_rt_json_reader_new(&src);
        assert_eq!(velt_rt_json_reader_expect_object_start(r), 1);
        let mut got = Vec::new();
        loop {
            let mut key = MaybeUninit::<VeltStr>::uninit();
            if velt_rt_json_reader_next_key(r, key.as_mut_ptr()) != STEP_MORE {
                break;
            }
            let key = key.assume_init();
            got.push((key.is_static(), owned_text(key)));
            assert_eq!(velt_rt_json_reader_skip_value(r), 1);
        }
        assert_eq!(
            got,
            [
                (true, "plain".into()),
                (false, "escA".into()),
                (true, String::new())
            ]
        );
        velt_rt_json_reader_free(r);
    }
}

#[test]
fn deep_nesting_is_skipped_without_recursion() {
    let depth = 200_000;
    let doc = format!(
        r#"{{"name":"n","age":1,"tags":[],"deep":{}{}}}"#,
        "[{\"k\":".repeat(depth) + "0",
        "}]".repeat(depth)
    );
    assert_eq!(decode_user(&doc).unwrap().name, "n");
    let bad = format!(
        r#"{{"name":"n","age":1,"tags":[],"deep":{}}}"#,
        "[".repeat(depth)
    );
    let err = decode_user(&bad).unwrap_err();
    assert!(
        err.starts_with("invalid JSON at $.deep: unexpected character '}'"),
        "{err}"
    );
}
