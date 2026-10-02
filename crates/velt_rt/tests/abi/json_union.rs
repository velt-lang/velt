//! A union decoder looking ahead for its discriminant, written the way the generated
//! `JSON.parse<Leaf | Node>` drives the reader (`mark`, `skip_lookahead`, `reset`): with the
//! tag last in every object, nested unions must stay linear in the document size.

use super::json_reader::borrow;
use crate::json::reader::*;
use crate::json::reader_abi::*;
use crate::str::{velt_rt_str_drop, VeltStr};
use std::mem::MaybeUninit;

/// `{ pad: string; child: Leaf | Node; kind: "node" }` or `{ v: i64; kind: "leaf" }`, the
/// tag last: a document `levels` nodes deep around one leaf.
fn nested(levels: usize) -> String {
    let pad = "x".repeat(100);
    let mut s = format!("{{\"pad\":\"{pad}\",\"child\":").repeat(levels);
    s.push_str("{\"v\":7,\"kind\":\"leaf\"}");
    s.push_str(&",\"kind\":\"node\"}".repeat(levels));
    s
}

/// Calls `f` with each key of the object at the reader (`{` included); false on an error.
unsafe fn each_key(r: *mut Reader, mut f: impl FnMut(&[u8]) -> bool) -> bool {
    if velt_rt_json_reader_expect_object_start(r) == 0 {
        return false;
    }
    loop {
        let mut key = MaybeUninit::<VeltStr>::uninit();
        match velt_rt_json_reader_next_key(r, key.as_mut_ptr()) {
            STEP_END => return true,
            STEP_MORE => {
                let mut key = key.assume_init();
                let ok = f(key.as_bytes());
                velt_rt_str_drop(&mut key);
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The `kind` of the object at the reader, read ahead; the reader is back at the `{` after.
unsafe fn lookahead_kind(r: *mut Reader) -> Option<String> {
    let mark = velt_rt_json_reader_mark(r);
    let mut kind = None;
    each_key(r, |key| {
        if key != b"kind" {
            return velt_rt_json_reader_skip_lookahead(r) == 1;
        }
        let mut s = MaybeUninit::uninit();
        if velt_rt_json_reader_read_string(r, s.as_mut_ptr()) == 0 {
            return false;
        }
        let mut s = s.assume_init();
        kind = Some(String::from_utf8_lossy(s.as_bytes()).into_owned());
        velt_rt_str_drop(&mut s);
        false // found: stop here
    });
    velt_rt_json_reader_reset(r, mark);
    kind
}

/// Decodes a `Leaf | Node`; the number of nodes above the leaf.
unsafe fn decode(r: *mut Reader) -> Option<usize> {
    let kind = lookahead_kind(r).filter(|k| k == "leaf" || k == "node")?;
    let mut depth = 0;
    let mut child_ok = true;
    let ok = each_key(r, |key| match key {
        b"child" if kind == "node" => match decode(r) {
            Some(d) => {
                depth = d + 1;
                true
            }
            None => {
                child_ok = false;
                false
            }
        },
        _ => velt_rt_json_reader_skip_value(r) == 1,
    });
    (ok && child_ok).then_some(depth)
}

fn parse(src: &str) -> Option<usize> {
    parse_counting(src).0
}

/// [`parse`], and how many bytes the lookahead walked (rather than jumped over).
fn parse_counting(src: &str) -> (Option<usize>, usize) {
    let s = borrow(src);
    unsafe {
        let r = velt_rt_json_reader_new(&s);
        let depth = decode(r).filter(|_| velt_rt_json_reader_end(r) == 1);
        let walked = (*r).lookahead_walked;
        velt_rt_json_reader_free(r);
        (depth, walked)
    }
}

/// Runs `f` on a thread with room for thousands of decoder frames.
fn with_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn nested_unions_decode() {
    assert_eq!(with_stack(|| parse(&nested(0))), Some(0));
    assert_eq!(with_stack(|| parse(&nested(3))), Some(3));
    // A tag the decoder does not know fails cleanly.
    assert_eq!(
        with_stack(|| parse(&nested(3).replace("leaf", "tree"))),
        None
    );
}

#[test]
fn union_lookahead_stays_linear() {
    // The lookahead walks each byte about once: `4 * n` levels walk about 4 times as much as
    // `n`, not 16 times (each level skipping its whole subtree again). Counted, not timed, so a
    // busy machine cannot make it flaky.
    let n = 4_000;
    let (small, large) = (nested(n), nested(4 * n));
    let ((d_small, w_small), (d_large, w_large)) =
        with_stack(move || (parse_counting(&small), parse_counting(&large)));
    assert_eq!((d_small, d_large), (Some(n), Some(4 * n)));
    assert!(
        w_large < w_small * 5,
        "{n} levels walked {w_small} bytes, {} walked {w_large}: not linear",
        4 * n
    );
}
