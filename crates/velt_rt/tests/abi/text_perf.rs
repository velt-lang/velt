//! Throughput of the string builder and the JSON reader/value parser (timings are printed; run
//! with `cargo test -p velt_rt --release --lib text_perf -- --nocapture` for real numbers).
//! Limits are strict in release builds and generous in debug builds.

use super::json_reader::{borrow, decode_user_object};
use crate::json::reader::*;
use crate::json::reader_abi::*;
use crate::json::value_abi::{velt_rt_json_parse_value, velt_rt_json_value_free};
use crate::str::{velt_rt_str_drop, VeltStr};
use crate::strbuf::*;
use std::mem::MaybeUninit;
use std::time::Instant;

#[test]
fn builder_1m_small_pushes() {
    let part = VeltStr::from_static(b"abc");
    let run = || {
        let t = Instant::now();
        let mut b = MaybeUninit::uninit();
        let mut out = MaybeUninit::uninit();
        unsafe {
            velt_rt_strbuf_new(0, b.as_mut_ptr());
            let mut b = b.assume_init();
            for i in 0..1_000_000i64 {
                velt_rt_strbuf_push_str(&mut b, &part);
                velt_rt_strbuf_push_i64(&mut b, i & 7);
            }
            velt_rt_strbuf_finish(&mut b, out.as_mut_ptr());
        }
        let elapsed = t.elapsed();
        let mut s = unsafe { out.assume_init() };
        assert_eq!(s.len(), 4_000_000);
        unsafe { velt_rt_str_drop(&mut s) };
        elapsed
    };
    run(); // warm up the allocator
    let elapsed = run();
    eprintln!("strbuf: 1M push_str + 1M push_i64: {elapsed:?}");
    let limit_ms = if cfg!(debug_assertions) { 2000 } else { 50 };
    assert!(elapsed.as_millis() < limit_ms, "builder took {elapsed:?}");
}

/// About `target` bytes of `[User, ...]` with escapes, unknown keys and nested values.
fn users_doc(target: usize) -> (String, usize) {
    let mut doc = String::with_capacity(target + 256);
    doc.push('[');
    let mut n = 0;
    while doc.len() < target {
        if n > 0 {
            doc.push(',');
        }
        doc += &format!(
            r#"{{"name":"user {n} \"q\"","age":{},"tags":["admin","t{n}"],"email":"u{n}@example.com","score":{}.25,"meta":{{"a":[1,2,3],"b":null}}}}"#,
            20 + n % 50,
            n * 7
        );
        n += 1;
    }
    doc.push(']');
    (doc, n)
}

fn mb_per_s(bytes: usize, t: std::time::Duration) -> f64 {
    bytes as f64 / 1e6 / t.as_secs_f64()
}

#[test]
fn json_reader_throughput_10mb() {
    let (doc, count) = users_doc(10 << 20);
    let src = borrow(&doc);
    // Pure tokenizing/validation: skip the whole document.
    let t = Instant::now();
    unsafe {
        let r = velt_rt_json_reader_new(&src);
        assert_eq!(velt_rt_json_reader_skip_value(r), 1);
        assert_eq!(velt_rt_json_reader_end(r), 1);
        velt_rt_json_reader_free(r);
    }
    let skip = t.elapsed();
    // A generated `JSON.parse<User[]>` decoder.
    let t = Instant::now();
    let users = unsafe {
        let r = velt_rt_json_reader_new(&src);
        assert_eq!(velt_rt_json_reader_expect_array_start(r), 1);
        let mut users = Vec::new();
        while velt_rt_json_reader_array_next(r) == STEP_MORE {
            users.push(decode_user_object(r).unwrap());
        }
        assert_eq!(velt_rt_json_reader_end(r), 1);
        velt_rt_json_reader_free(r);
        users
    };
    let decode = t.elapsed();
    assert_eq!(users.len(), count);
    assert_eq!(users[3].name, "user 3 \"q\"");
    // JSON.parseValue: the whole tree.
    let t = Instant::now();
    unsafe {
        let (mut h, mut err) = (MaybeUninit::uninit(), MaybeUninit::uninit());
        assert_eq!(
            velt_rt_json_parse_value(&src, h.as_mut_ptr(), err.as_mut_ptr()),
            1
        );
        velt_rt_json_value_free(h.assume_init());
    }
    let value = t.elapsed();
    let len = doc.len();
    eprintln!(
        "json {:.1} MB, {count} users: skip_value {:.0} MB/s, typed decode {:.0} MB/s, parseValue+free {:.0} MB/s",
        len as f64 / 1e6,
        mb_per_s(len, skip),
        mb_per_s(len, decode),
        mb_per_s(len, value)
    );
    if !cfg!(debug_assertions) {
        assert!(mb_per_s(len, skip) > 200.0 && mb_per_s(len, decode) > 100.0);
    }
}
