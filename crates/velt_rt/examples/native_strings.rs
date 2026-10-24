//! How a native library's string results reach Velt, per path (native_abi.md "Strings"):
//!
//! - `vec+copy`: encode into a `Vec<u8>`, close the array (a copy), make the string from it (a
//!   copy and a count): what the runtime's own Postgres driver did;
//! - `vec+check+copy`: the same with a `String` result through the table (`String::from_utf8`,
//!   then `str_new` checks again): a native library before `StrBuf`;
//! - `strbuf`: encode into a `StrBuf`, the Velt string itself (one count at the end).
//!
//! Run: `cargo run --release -p velt_rt --example native_strings [name filter]`.
//!
//! It runs as a Velt program: the runtime's own `main` starts it (as for a compiled program,
//! with the native table set up) and calls `velt_main` below.

#![no_main]

use std::hint::black_box;
use std::time::Instant;

use velt_native::{api, StrBuf, VeltStr};
use velt_rt as _;
use velt_text::db_json::RowWriter;
use velt_text::Out;

/// Writes a result set the way the drivers do: `rows` rows of `cols` (int, text) pairs.
fn rows<O: Out>(out: O, array: bool, rows: usize, text: &str) -> O {
    let mut w = RowWriter::with_out(out, ["id", "message"], array);
    for i in 0..rows {
        w.begin_row();
        w.int(i as i64 * 7919);
        w.text(text.as_bytes());
        w.end_row();
    }
    w.finish()
}

/// Writes `len` bytes of `piece`s (text a library builds in small writes).
fn text<O: Out>(mut out: O, len: usize, piece: &str) -> O {
    let mut n = 0;
    while n + piece.len() <= len {
        out.extend(piece.as_bytes());
        n += piece.len();
    }
    out
}

/// Rows were written without the brackets and `into_array` copied them in (`wrap`).
fn close(v: Vec<u8>, wrap: bool) -> Vec<u8> {
    if !wrap {
        return v;
    }
    let mut arr = Vec::with_capacity(v.len() + 2);
    arr.push(b'[');
    arr.extend_from_slice(&v);
    arr.push(b']');
    arr
}

fn vec_copy(make: &dyn Fn(Vec<u8>) -> Vec<u8>, wrap: bool) -> VeltStr {
    let arr = close(make(Vec::with_capacity(64)), wrap);
    let mut s = VeltStr::empty();
    unsafe { (api().str_from_utf8)(arr.as_ptr(), arr.len(), &mut s) };
    s
}

fn vec_check_copy(make: &dyn Fn(Vec<u8>) -> Vec<u8>, wrap: bool) -> VeltStr {
    let arr = close(make(Vec::with_capacity(64)), wrap);
    let s = String::from_utf8(arr).unwrap();
    let mut out = VeltStr::empty();
    unsafe { (api().str_new)(s.as_ptr(), s.len(), &mut out) };
    out
}

fn strbuf(make: &dyn Fn(StrBuf) -> StrBuf) -> velt_native::Str {
    make(StrBuf::new()).finish()
}

/// Nanoseconds per call: the best of 7 runs of enough calls for ~20 ms each.
fn time<T>(f: impl Fn() -> T) -> f64 {
    let mut n = 1u64;
    loop {
        let t = Instant::now();
        for _ in 0..n {
            drop(black_box(f()));
        }
        if t.elapsed().as_millis() >= 20 {
            break;
        }
        n *= 2;
    }
    (0..7)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..n {
                drop(black_box(f()));
            }
            t.elapsed().as_nanos() as f64 / n as f64
        })
        .fold(f64::INFINITY, f64::min)
}

struct Drop3(VeltStr);
impl Drop for Drop3 {
    fn drop(&mut self) {
        unsafe { (api().str_drop)(&mut self.0) };
    }
}

fn case(name: &str, wrap: bool, vec: &dyn Fn(Vec<u8>) -> Vec<u8>, buf: &dyn Fn(StrBuf) -> StrBuf) {
    if std::env::args().nth(1).is_some_and(|f| !name.contains(&f)) {
        return;
    }
    let len = buf(StrBuf::new()).len();
    if std::env::var_os("NS_ONLY_STRBUF").is_some() {
        println!(
            "{name:<28} {len:>8} B  strbuf {:>10.1} ns",
            time(|| strbuf(buf))
        );
        return;
    }
    let a = time(|| Drop3(vec_copy(vec, wrap)));
    let b = time(|| Drop3(vec_check_copy(vec, wrap)));
    let c = time(|| strbuf(buf));
    println!(
        "{name:<28} {len:>8} B  vec+copy {a:>10.1} ns  vec+check+copy {b:>10.1} ns  strbuf {c:>10.1} ns  ({:+.0}% / {:+.0}%)",
        (c / a - 1.0) * 100.0,
        (c / b - 1.0) * 100.0
    );
}

/// A string argument an async export keeps: copied into a `String` (`str_bytes`, a UTF-8
/// check, an allocation) or retained as a `Str` (a count).
fn param(name: &str, text: &str) {
    if std::env::args().nth(1).is_some_and(|f| !name.contains(&f)) {
        return;
    }
    let s = velt_native::Str::new(text);
    let raw = &s as *const velt_native::Str as *const VeltStr;
    let copy = time(|| {
        let mut len = 0usize;
        let p = unsafe { (api().str_bytes)(raw, &mut len) };
        let b = unsafe { std::slice::from_raw_parts(p, len) };
        String::from_utf8_lossy(b).into_owned()
    });
    let retain = time(|| s.clone());
    println!(
        "{name:<28} {:>8} B  String {copy:>10.1} ns  Str {retain:>10.1} ns  ({:+.0}%)",
        text.len(),
        (retain / copy - 1.0) * 100.0
    );
}

/// The program the runtime runs.
#[no_mangle]
pub extern "C" fn velt_main() -> i32 {
    for (name, n) in [
        ("param: 16", 16),
        ("param: 200", 200),
        ("param: 1K", 1024),
        ("param: 64K", 65536),
    ] {
        param(name, &"0123456789abcdef".repeat(n / 16));
    }
    const FORTUNE: &str =
        "A computer program does what you tell it to do, not what you want it to do.";
    const JAPANESE: &str = "フレームワークのベンチマーク";
    for (name, n, msg) in [
        ("rows: 1 (/db)", 1, "x"),
        ("rows: 12 fortunes ascii", 12, FORTUNE),
        ("rows: 12 fortunes + japanese", 12, JAPANESE),
        ("rows: 20 (queries=20)", 20, "x"),
        ("rows: 500 (queries=500)", 500, "x"),
    ] {
        case(name, true, &move |v| rows(v, false, n, msg), &move |b| {
            rows(b, true, n, msg)
        });
    }
    for (name, len, piece) in [
        ("text: 16 ascii", 16, "0123456789abcdef"),
        ("text: 200 ascii", 200, "0123456789abcdef"),
        ("text: 1K ascii", 1024, "0123456789abcdef"),
        ("text: 1K non-ascii", 1024, "0123456789abcdé"),
        ("text: 64K ascii", 65536, "0123456789abcdef"),
        ("text: 1M ascii", 1 << 20, "0123456789abcdef"),
        ("text: 1M non-ascii", 1 << 20, "0123456789abcdé"),
    ] {
        case(name, false, &move |v| text(v, len, piece), &move |b| {
            text(b, len, piece)
        });
    }
    0
}
