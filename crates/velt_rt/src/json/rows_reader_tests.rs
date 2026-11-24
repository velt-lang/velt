//! Tests of the rows reader (rows_reader.rs) over binary rows.
use super::bin::BinRows;
use super::*;

/// A reader over `buf`.
fn make(buf: Vec<u8>, flags: u32) -> RowReader {
    RowReader::new(buf, flags)
}

/// Two rows of every kind of cell.
fn sample(single: bool) -> Vec<u8> {
    let mut w = BinRows::new(["id", "name", "score", "ok", "data", "nöte"], single);
    w.int(7);
    w.text(b"ada \"x\"");
    w.float(1.5);
    w.bool(true);
    w.bytes(&[1, 255]);
    w.null();
    w.end_row();
    if !single {
        w.int(-1);
        w.text(b"");
        w.float(3.0);
        w.bool(false);
        w.bytes(&[]);
        w.text("é".as_bytes());
        w.end_row();
    }
    w.finish()
}

fn key(r: &mut RowReader) -> Option<String> {
    r.next_key().unwrap().map(|k| {
        // SAFETY: a key the reader just made; released once.
        unsafe {
            let text = k.to_string_lossy();
            let mut k = k;
            crate::str::velt_rt_str_drop(&mut k);
            text
        }
    })
}

fn text(r: &mut RowReader) -> String {
    let s = r.string().unwrap();
    // SAFETY: as in `key`.
    unsafe {
        let text = s.to_string_lossy();
        let mut s = s;
        crate::str::velt_rt_str_drop(&mut s);
        text
    }
}

#[test]
fn reads_rows_as_an_array_of_objects() {
    let mut r = make(sample(false), 0);
    assert_eq!(r.peek(), TOKEN_ARRAY_START);
    assert_eq!(r.open(b'['), 1);
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.peek(), TOKEN_OBJECT_START);
    assert_eq!(r.open(b'{'), 1);
    assert_eq!(key(&mut r).as_deref(), Some("id"));
    assert_eq!(r.peek(), TOKEN_NUMBER);
    assert_eq!(r.i64(), Some(7));
    assert_eq!(key(&mut r).as_deref(), Some("name"));
    assert_eq!(text(&mut r), "ada \"x\"");
    assert_eq!(key(&mut r).as_deref(), Some("score"));
    assert_eq!(r.f64(), Some(1.5));
    assert_eq!(key(&mut r).as_deref(), Some("ok"));
    assert_eq!(r.peek(), TOKEN_TRUE);
    assert_eq!(r.bool(), Some(true));
    assert_eq!(key(&mut r).as_deref(), Some("data"));
    assert_eq!(r.peek(), TOKEN_ARRAY_START);
    assert_eq!(r.open(b'['), 1);
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.i64(), Some(1));
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.f64(), Some(255.0));
    assert_eq!(r.array_next(), STEP_END);
    assert_eq!(key(&mut r).as_deref(), Some("nöte"));
    assert_eq!(r.peek(), TOKEN_NULL);
    assert_eq!(r.null(), 1);
    assert_eq!(key(&mut r), None);
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.open(b'{'), 1);
    assert_eq!(key(&mut r).as_deref(), Some("id"));
    assert_eq!(r.f64(), Some(-1.0));
    assert_eq!(key(&mut r).as_deref(), Some("name"));
    assert_eq!(text(&mut r), "");
    assert_eq!(key(&mut r).as_deref(), Some("score"));
    // An integral float is an integer too, as `3.0` is in JSON text.
    assert_eq!(r.i64(), Some(3));
    assert_eq!(key(&mut r).as_deref(), Some("ok"));
    assert_eq!(r.bool(), Some(false));
    assert_eq!(key(&mut r).as_deref(), Some("data"));
    assert_eq!(r.open(b'['), 1);
    assert_eq!(r.array_next(), STEP_END);
    assert_eq!(key(&mut r).as_deref(), Some("nöte"));
    assert_eq!(text(&mut r), "é");
    assert_eq!(key(&mut r), None);
    assert_eq!(r.array_next(), STEP_END);
    assert_eq!(r.peek(), TOKEN_EOF);
    assert_eq!(r.end(), 1);
}

#[test]
fn a_single_row_is_an_object() {
    let mut r = make(sample(true), 0);
    assert_eq!(r.peek(), TOKEN_OBJECT_START);
    assert_eq!(r.open(b'['), 0);
    assert_eq!(r.message("array", "$"), "expected array at $");
    let mut r = make(sample(true), 0);
    assert_eq!(r.open(b'{'), 1);
    for _ in 0..6 {
        assert!(key(&mut r).is_some());
        assert_eq!(r.skip(), 1);
    }
    assert_eq!(key(&mut r), None);
    assert_eq!(r.end(), 1);
}

#[test]
fn no_rows_is_an_empty_array() {
    let mut r = make(BinRows::new(["a"], false).finish(), 0);
    assert_eq!(r.open(b'['), 1);
    assert_eq!(r.peek(), TOKEN_ARRAY_END);
    assert_eq!(r.array_next(), STEP_END);
    assert_eq!(r.end(), 1);
}

#[test]
fn a_wrong_kind_fails_until_reset() {
    let mut r = make(sample(false), 0);
    r.open(b'[');
    r.array_next();
    r.open(b'{');
    key(&mut r);
    let mark = r.mark();
    assert!(r.string().is_none());
    assert_eq!(r.peek(), TOKEN_ERROR);
    assert_eq!(r.i64(), None);
    assert_eq!(r.message("string", "$[0].id"), "expected string at $[0].id");
    r.reset(mark);
    assert_eq!(r.i64(), Some(7));
    key(&mut r);
    // A non-integral float is no integer; a bool no number; text no bool.
    assert_eq!(r.bool(), None);
    r.reset(mark);
    assert_eq!(r.skip(), 1);
    assert_eq!(key(&mut r).as_deref(), Some("name"));
    assert_eq!(r.skip(), 1);
    key(&mut r);
    assert_eq!(r.i64(), None);
}

#[test]
fn unknown_keys_are_skipped_or_rejected() {
    let mut r = make(sample(true), 0);
    r.open(b'{');
    key(&mut r);
    assert_eq!(r.skip_unknown(), 1);
    assert_eq!(key(&mut r).as_deref(), Some("name"));
    let mut r = make(sample(true), FLAG_REJECT_UNKNOWN);
    r.open(b'{');
    key(&mut r);
    assert_eq!(r.skip_unknown(), 0);
    assert_eq!(r.message("", "$.id"), "unknown field at $.id");
}

#[test]
fn whole_rows_skip_and_read_as_values() {
    let mut r = make(sample(false), 0);
    r.open(b'[');
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.skip(), 1);
    assert_eq!(r.array_next(), STEP_MORE);
    let row = json_of(&r.value().unwrap());
    assert!(row.starts_with("{\"") && row.ends_with('}'), "{row}");
    assert_eq!(row.matches("\":").count(), 6, "{row}");
    assert_eq!(r.array_next(), STEP_END);
    let mut r = make(sample(false), 0);
    let rows = json_of(&r.value().unwrap());
    assert!(rows.starts_with("[{") && rows.ends_with("}]"), "{rows}");
    assert_eq!(rows.matches("},{").count(), 1, "{rows}");
    assert_eq!(r.end(), 1);
}

#[test]
fn negative_zero_reads_as_zero() {
    // The JSON text these rows stand for writes -0.0 as `0`.
    let mut w = BinRows::new(["x"], true);
    w.float(-0.0);
    w.end_row();
    let buf = w.finish();
    assert_eq!(bin::to_json(&buf).unwrap(), b"{\"x\":0}");
    let mut r = make(buf, 0);
    r.open(b'{');
    key(&mut r);
    assert_eq!(r.f64().map(f64::is_sign_negative), Some(false));
}

#[test]
fn a_column_name_of_any_length_is_kept() {
    let long = format!("{}z", "a".repeat(70_000));
    let mut w = BinRows::new([long.as_str(), "ü".repeat(40_000).as_str()], true);
    w.int(1);
    w.int(2);
    w.end_row();
    let mut r = make(w.finish(), 0);
    r.open(b'{');
    assert_eq!(key(&mut r), Some(long));
    assert_eq!(r.i64(), Some(1));
    assert_eq!(key(&mut r), Some("ü".repeat(40_000)));
    assert_eq!(r.i64(), Some(2));
    assert_eq!(key(&mut r), None);
    assert_eq!(r.end(), 1);
}

#[test]
fn marks_made_in_a_row_are_forgotten_at_the_next() {
    let mut w = BinRows::new(["id"], false);
    for i in 0..1000 {
        w.int(i);
        w.end_row();
    }
    let mut r = make(w.finish(), 0);
    let top = r.mark();
    r.open(b'[');
    for i in 0..1000 {
        assert_eq!(r.array_next(), STEP_MORE);
        // A union decoder looks ahead in each row and reads it again.
        let mark = r.mark();
        r.open(b'{');
        key(&mut r);
        assert!(r.string().is_none());
        r.reset(mark);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.i64(), Some(i));
        assert_eq!(key(&mut r), None);
        assert!(r.marks.len() <= 2);
    }
    assert_eq!(r.array_next(), STEP_END);
    // A mark made before the rows still reads them again.
    r.reset(top);
    r.open(b'[');
    assert_eq!(r.array_next(), STEP_MORE);
    r.open(b'{');
    key(&mut r);
    assert_eq!(r.i64(), Some(0));
}

#[test]
fn a_damaged_buffer_is_a_mismatch_not_a_crash() {
    let whole = sample(false);
    for cut in 0..whole.len() {
        let mut r = make(whole[..cut].to_vec(), 0);
        if r.open(b'[') == 0 {
            continue;
        }
        while r.array_next() == STEP_MORE {
            if r.open(b'{') == 0 {
                break;
            }
            while let Ok(Some(k)) = r.next_key() {
                let mut k = k;
                // SAFETY: a key the reader just made; released once.
                unsafe { crate::str::velt_rt_str_drop(&mut k) };
                if r.skip() == 0 {
                    break;
                }
            }
        }
    }
}

fn json_of(v: &[u8]) -> String {
    String::from_utf8(v.to_vec()).unwrap()
}

/// A column holding an array or object (a Postgres array, `jsonb`) reads as the JSON it is.
#[test]
fn nested_values_read_as_their_json() {
    let mut w = BinRows::new(["tags", "meta", "n"], true);
    w.json(br#"[1, "two", [3]]"#);
    w.json(br#"{"a": {"b": true}, "c": null}"#);
    w.int(9);
    w.end_row();
    let buf = w.finish();

    let mut r = make(buf.clone(), 0);
    assert_eq!(r.open(b'{'), 1);
    assert_eq!(key(&mut r).as_deref(), Some("tags"));
    assert_eq!(r.peek(), TOKEN_ARRAY_START);
    assert_eq!(r.open(b'['), 1);
    assert_eq!(r.array_next(), STEP_MORE);
    // A look-ahead inside the nested value, read again.
    let m = r.mark();
    assert_eq!(r.i64(), Some(1));
    r.reset(m);
    assert_eq!(r.i64(), Some(1));
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(text(&mut r), "two");
    assert_eq!(r.array_next(), STEP_MORE);
    assert_eq!(r.skip(), 1);
    assert_eq!(r.array_next(), STEP_END);
    // The value read, the next column follows.
    assert_eq!(key(&mut r).as_deref(), Some("meta"));
    assert_eq!(r.open(b'{'), 1);
    assert_eq!(key(&mut r).as_deref(), Some("a"));
    let v = r.value().unwrap();
    // The nested value's text as the driver wrote it (std parses it).
    assert_eq!(json_of(&v), r#"{"b": true}"#);
    assert_eq!(key(&mut r).as_deref(), Some("c"));
    assert_eq!(r.null(), 1);
    assert_eq!(key(&mut r), None);
    assert_eq!(key(&mut r).as_deref(), Some("n"));
    assert_eq!(r.i64(), Some(9));
    assert_eq!(key(&mut r), None);
    assert_eq!(r.end(), 1);

    // Skipped whole, read whole as a value, or not the kind asked for.
    let mut r = make(buf.clone(), 0);
    r.open(b'{');
    key(&mut r);
    assert_eq!(r.skip(), 1);
    key(&mut r);
    assert_eq!(
        json_of(&r.value().unwrap()),
        r#"{"a": {"b": true}, "c": null}"#
    );
    key(&mut r);
    assert_eq!(r.i64(), Some(9));
    let mut r = make(buf, 0);
    r.open(b'{');
    key(&mut r);
    assert!(r.string().is_none());
    assert!(r.message("string", "$.tags").contains("expected string"));
}
