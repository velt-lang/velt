//! The string operations through the C ABI, checked against the UTF-16 reference model
//! (utf16_model.rs, #377) on random inputs: random strings in every form (static, inline, heap),
//! with lengths around the inline limit (22, 23 bytes) and the breadcrumb stride (64, 128 units),
//! and random operations on them.
//!
//! The runtime counts UTF-8 bytes until phase 2 of #377, where only ASCII agrees with the model,
//! so the runtime is checked on [`RUNTIME_ALPHABETS`]. Phase 2 adds the other alphabets; the model
//! checks below already run on all of them. Every runtime string the checks touch must also carry
//! its model length as its unit count (phase 1), on every alphabet the runtime can already hold
//! ([`WELL_FORMED_ALPHABETS`]).
//!
//! `VELT_UTF16_SEED` replays a failing run (the failure message prints the seed);
//! `VELT_UTF16_CASES` changes the number of operations.

use super::utf16_model::{self as model, show, wtf8_decode, wtf8_encode};
use crate::hash::velt_rt_str_hash;
use crate::str::{velt_rt_str_cmp, velt_rt_str_concat, velt_rt_str_drop, VeltStr};
use crate::str_array::{velt_rt_str_array_drop, VeltStrArray};
use crate::str_ops::replace::*;
use crate::str_ops::search::*;
use crate::str_ops::slice::*;
use crate::str_ops::split::*;
use crate::strbuf::{velt_rt_strbuf_new, velt_rt_strbuf_push_str};
use std::mem::MaybeUninit;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Alphabet {
    /// Printable ASCII and a few controls.
    Ascii,
    /// Two- and three-byte code points, including U+E000–U+FFFF (where byte order and code-unit
    /// order disagree).
    Bmp,
    /// Supplementary code points (a surrogate pair each).
    Astral,
    /// Lone surrogates, high and low.
    Lone,
}

const ALL_ALPHABETS: &[Alphabet] = &[
    Alphabet::Ascii,
    Alphabet::Bmp,
    Alphabet::Astral,
    Alphabet::Lone,
];

/// The alphabets on which the runtime agrees with the model. Phase 2 of #377 makes this
/// `ALL_ALPHABETS`.
const RUNTIME_ALPHABETS: &[Alphabet] = &[Alphabet::Ascii];

/// The alphabets the runtime holds before phase 2 of #377 (it can't make lone surrogates yet).
const WELL_FORMED_ALPHABETS: &[Alphabet] = &[Alphabet::Ascii, Alphabet::Bmp, Alphabet::Astral];

/// Lengths in code units around the inline limit and the breadcrumb stride; other lengths are
/// random up to 200.
const LENGTHS: &[usize] = &[0, 1, 2, 3, 21, 22, 23, 24, 63, 64, 65, 127, 128, 129];

/// xorshift64*: small, deterministic and good enough to pick test inputs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// Push one character (one or two code units) of `alphabet`.
fn push_char(rng: &mut Rng, alphabet: Alphabet, s: &mut Vec<u16>) {
    match alphabet {
        Alphabet::Ascii => s.push(*rng.pick(b"abc ,$&'`\t\nxyzAZ09") as u16),
        Alphabet::Bmp => {
            s.push(*rng.pick(&[0xE9, 0xF6, 0x20AC, 0x65E5, 0x672C, 0xE000, 0xFF5E, 0xFFFD]))
        }
        Alphabet::Astral => {
            let cp: u32 = *rng.pick(&[0x1F600, 0x1F680, 0x10000, 0x10FFFF, 0x1D11E]);
            s.push((0xD800 + ((cp - 0x10000) >> 10)) as u16);
            s.push((0xDC00 + ((cp - 0x10000) & 0x3FF)) as u16);
        }
        Alphabet::Lone => s.push(*rng.pick(&[0xD83D, 0xDE00, 0xD800, 0xDBFF, 0xDC00, 0xDFFF])),
    }
}

/// Push one character of a random one of `alphabets`.
fn push_any(rng: &mut Rng, alphabets: &[Alphabet], s: &mut Vec<u16>) {
    let a = *rng.pick(alphabets);
    push_char(rng, a, s);
}

/// A random string over `alphabets`: usually mostly the first one, sometimes an even mix.
fn gen_string(rng: &mut Rng, alphabets: &[Alphabet]) -> Vec<u16> {
    let len = if rng.chance(70) {
        *rng.pick(LENGTHS)
    } else {
        rng.below(200)
    };
    let main = *rng.pick(alphabets);
    let mixed = rng.chance(30);
    let mut s = Vec::with_capacity(len + 1);
    while s.len() < len {
        let a = if mixed || rng.chance(10) {
            *rng.pick(alphabets)
        } else {
            main
        };
        push_char(rng, a, &mut s);
    }
    s
}

/// A needle for `s`: often a piece of `s` (so that searches find it), else a short random string.
/// Without [`Alphabet::Lone`] the piece never cuts a surrogate pair.
fn gen_needle(rng: &mut Rng, s: &[u16], alphabets: &[Alphabet]) -> Vec<u16> {
    if !s.is_empty() && rng.chance(60) {
        let mut a = rng.below(s.len() + 1);
        let mut b = (a + rng.below(4)).min(s.len());
        if !alphabets.contains(&Alphabet::Lone) {
            let inside_pair = |i: usize| i > 0 && i < s.len() && (0xDC00..0xE000).contains(&s[i]);
            a -= inside_pair(a) as usize;
            b += inside_pair(b) as usize;
        }
        return s[a..b].to_vec();
    }
    let mut n = Vec::new();
    for _ in 0..rng.below(3) {
        push_any(rng, alphabets, &mut n);
    }
    n
}

/// A position argument: mostly in range, sometimes negative, past the end or omitted.
fn gen_pos(rng: &mut Rng, len: usize) -> i64 {
    match rng.below(10) {
        0 => -(rng.below(len + 3) as i64),
        1 => len as i64 + rng.below(3) as i64,
        2 => i64::MAX,
        _ => rng.below(len + 1) as i64,
    }
}

/// A runtime string that is dropped at the end of its scope.
struct Rt(VeltStr);

impl Drop for Rt {
    fn drop(&mut self) {
        unsafe { velt_rt_str_drop(&mut self.0) };
    }
}

/// The model string `s` as a runtime string, in a random form.
fn to_rt(rng: &mut Rng, s: &[u16]) -> Rt {
    let bytes = wtf8_encode(s);
    let r = Rt(match rng.below(3) {
        // A literal or a borrowed sub-range (static form). Leaked: tests only.
        0 => VeltStr::from_static(Box::leak(bytes.into_boxed_slice())),
        // Inline when it fits, else a heap buffer of exactly its size.
        1 => VeltStr::from_bytes(&bytes),
        // A heap buffer with spare room, as a builder leaves it.
        _ => {
            let mut h = VeltStr::with_capacity(24 + bytes.len() + rng.below(16));
            unsafe { h.push_wtf8(&bytes, None) };
            h
        }
    });
    assert_eq!(units(&r.0), s);
    r
}

/// The code units of a runtime string; panics unless its bytes are canonical WTF-8 and the unit
/// count stored in the value is their number.
fn units(s: &VeltStr) -> Vec<u16> {
    let u = wtf8_decode(unsafe { s.as_bytes() });
    assert_eq!(
        s.units(),
        u.len(),
        "stored unit count of {} ({s:?})",
        show(&u)
    );
    u
}

/// `s.length` as compiled code reads it. Bytes until phase 2 of #377, then code units.
fn rt_length(s: &VeltStr) -> usize {
    s.len()
}

fn out_str(f: impl FnOnce(*mut VeltStr)) -> Vec<u16> {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    f(out.as_mut_ptr());
    let r = Rt(unsafe { out.assume_init() });
    units(&r.0)
}

fn split_rt(s: &VeltStr, sep: &VeltStr) -> Vec<Vec<u16>> {
    let mut out = MaybeUninit::<VeltStrArray>::uninit();
    unsafe { velt_rt_str_split(s, sep, out.as_mut_ptr()) };
    let mut arr = unsafe { out.assume_init() };
    let items = (0..arr.len as usize)
        .map(|i| units(unsafe { &*arr.ptr.add(i) }))
        .collect();
    unsafe { velt_rt_str_array_drop(&mut arr) };
    items
}

/// One random operation on random inputs; returns a description of it and whether the runtime
/// agreed with the model.
fn check_one(rng: &mut Rng, alphabets: &[Alphabet]) -> Result<(), String> {
    let s = gen_string(rng, alphabets);
    let n = gen_needle(rng, &s, alphabets);
    let (rs, rn) = (to_rt(rng, &s), to_rt(rng, &n));
    let (rs, rn) = (&rs.0, &rn.0);
    fn diff<T: PartialEq + std::fmt::Debug>(op: String, got: T, want: T) -> Result<(), String> {
        if got == want {
            Ok(())
        } else {
            Err(format!("{op}: runtime {got:?}, model {want:?}"))
        }
    }
    unsafe {
        match rng.below(16) {
            0 => diff(format!("{}.length", show(&s)), rt_length(rs), s.len()),
            1 => {
                let (a, b) = (gen_pos(rng, s.len()), gen_pos(rng, s.len()));
                let got = out_str(|o| velt_rt_str_slice(rs, a, b, o));
                diff(
                    format!("{}.slice({a}, {b})", show(&s)),
                    got,
                    model::slice(&s, a, b),
                )
            }
            2 => {
                let i = gen_pos(rng, s.len());
                let got = velt_rt_str_char_code_at(rs, i);
                diff(
                    format!("{}.charCodeAt({i})", show(&s)),
                    got,
                    model::char_code_at(&s, i),
                )
            }
            3 => {
                let from = gen_pos(rng, s.len());
                let op = format!("{}.indexOf({}, {from})", show(&s), show(&n));
                diff(
                    op,
                    velt_rt_str_index_of(rs, rn, from),
                    model::index_of(&s, &n, from),
                )
            }
            4 => {
                let from = gen_pos(rng, s.len());
                let op = format!("{}.lastIndexOf({}, {from})", show(&s), show(&n));
                diff(
                    op,
                    velt_rt_str_last_index_of(rs, rn, from),
                    model::last_index_of(&s, &n, from),
                )
            }
            5 => {
                let m = format!("{} / {}", show(&s), show(&n));
                diff(
                    format!("includes {m}"),
                    velt_rt_str_includes(rs, rn) != 0,
                    model::includes(&s, &n),
                )?;
                diff(
                    format!("startsWith {m}"),
                    velt_rt_str_starts_with(rs, rn) != 0,
                    model::starts_with(&s, &n),
                )?;
                diff(
                    format!("endsWith {m}"),
                    velt_rt_str_ends_with(rs, rn) != 0,
                    model::ends_with(&s, &n),
                )
            }
            6 => {
                // Compare with a near neighbour as often as with an unrelated string.
                let t = if rng.chance(50) {
                    gen_string(rng, alphabets)
                } else {
                    mutate(rng, &s, alphabets)
                };
                let rt = to_rt(rng, &t);
                let got = velt_rt_str_cmp(rs, &rt.0).signum();
                diff(
                    format!("cmp {} {}", show(&s), show(&t)),
                    got,
                    model::cmp(&s, &t),
                )
            }
            7 => {
                // Equal text built two ways (whole, and concatenated halves) is equal and hashes
                // the same.
                let cut = rng.below(s.len() + 1);
                let (a, b) = (to_rt(rng, &s[..cut]), to_rt(rng, &s[cut..]));
                let mut joined = MaybeUninit::<VeltStr>::uninit();
                velt_rt_str_concat(&a.0, &b.0, joined.as_mut_ptr());
                let joined = Rt(joined.assume_init());
                let op = format!("{} split at {cut} and joined", show(&s));
                diff(format!("{op}: text"), units(&joined.0), s.clone())?;
                diff(format!("{op}: =="), velt_rt_str_eq(rs, &joined.0), 1)?;
                diff(
                    format!("{op}: hash"),
                    velt_rt_str_hash(rs),
                    velt_rt_str_hash(&joined.0),
                )
            }
            8 => {
                let got = out_str(|o| velt_rt_str_concat(rs, rn, o));
                diff(
                    format!("{} + {}", show(&s), show(&n)),
                    got,
                    [s.clone(), n.clone()].concat(),
                )
            }
            9 => {
                // A builder fed several pieces, as template literals and `Array.join` use it.
                let pieces: Vec<Vec<u16>> = (0..1 + rng.below(5))
                    .map(|_| gen_needle(rng, &s, alphabets))
                    .collect();
                let mut buf = MaybeUninit::<VeltStr>::uninit();
                velt_rt_strbuf_new(rng.below(40) as u64, buf.as_mut_ptr());
                let mut buf = Rt(buf.assume_init());
                for p in &pieces {
                    let rp = to_rt(rng, p);
                    velt_rt_strbuf_push_str(&mut buf.0, &rp.0);
                }
                let op = format!(
                    "builder of {:?}",
                    pieces.iter().map(|p| show(p)).collect::<Vec<_>>()
                );
                diff(op, units(&buf.0), pieces.concat())
            }
            10 => {
                let k = rng.below(5) as i64 - 1;
                let mut out = MaybeUninit::<VeltStr>::uninit();
                let ok = velt_rt_str_repeat(rs, k, out.as_mut_ptr()) != 0;
                let got = Rt(out.assume_init());
                let got = ok.then(|| units(&got.0));
                diff(
                    format!("{}.repeat({k})", show(&s)),
                    got,
                    model::repeat(&s, k),
                )
            }
            11 | 12 => {
                let to = gen_needle(rng, &s, alphabets);
                let rto = to_rt(rng, &to);
                let all = rng.chance(50);
                let got = out_str(|o| {
                    if all {
                        velt_rt_str_replace_all(rs, rn, &rto.0, o)
                    } else {
                        velt_rt_str_replace(rs, rn, &rto.0, o)
                    }
                });
                let want = if all {
                    model::replace_all(&s, &n, &to)
                } else {
                    model::replace(&s, &n, &to)
                };
                let name = if all { "replaceAll" } else { "replace" };
                diff(
                    format!("{}.{name}({}, {})", show(&s), show(&n), show(&to)),
                    got,
                    want,
                )
            }
            13 => diff(
                format!("{}.split({})", show(&s), show(&n)),
                split_rt(rs, rn),
                model::split(&s, &n),
            ),
            14 => {
                let target = rng.below(s.len() + 8) as i64 - 2;
                let start = rng.chance(50);
                let got = out_str(|o| {
                    if start {
                        velt_rt_str_pad_start(rs, target, rn, o)
                    } else {
                        velt_rt_str_pad_end(rs, target, rn, o)
                    }
                });
                let want = if start {
                    model::pad_start(&s, target, &n)
                } else {
                    model::pad_end(&s, target, &n)
                };
                let name = if start { "padStart" } else { "padEnd" };
                diff(
                    format!("{}.{name}({target}, {})", show(&s), show(&n)),
                    got,
                    want,
                )
            }
            _ => {
                // A code unit of the alphabets, sometimes outside 0..=0xFFFF (`ToUint16`).
                let mut c = Vec::new();
                push_any(rng, alphabets, &mut c);
                let code = c[0] as i64
                    + if rng.chance(20) {
                        0x10000 * (rng.below(3) as i64 - 1)
                    } else {
                        0
                    };
                let got = out_str(|o| velt_rt_str_from_char_code(code, o));
                diff(
                    format!("String.fromCharCode({code})"),
                    got,
                    model::from_char_code(code),
                )
            }
        }
    }
}

/// `s` with one code unit changed, inserted or removed: a string that shares a prefix with `s`.
fn mutate(rng: &mut Rng, s: &[u16], alphabets: &[Alphabet]) -> Vec<u16> {
    let mut t = s.to_vec();
    let at = rng.below(t.len() + 1);
    let mut c = Vec::new();
    push_any(rng, alphabets, &mut c);
    match rng.below(3) {
        0 if at < t.len() => {
            t.splice(at..at + 1, c);
        }
        1 if at < t.len() => {
            t.remove(at);
        }
        _ => {
            t.splice(at..at, c);
        }
    }
    t
}

fn run(
    name: &str,
    alphabets: &[Alphabet],
    default_cases: u64,
    check: impl Fn(&mut Rng) -> Result<(), String>,
) {
    let seed = env_u64("VELT_UTF16_SEED").unwrap_or(0x5EED_0377);
    let cases = env_u64("VELT_UTF16_CASES").unwrap_or(default_cases);
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    for case in 0..cases {
        if let Err(e) = check(&mut rng) {
            panic!("{name} over {alphabets:?}, case {case} (VELT_UTF16_SEED={seed}): {e}");
        }
    }
}

#[test]
fn runtime_matches_model() {
    run("runtime vs model", RUNTIME_ALPHABETS, 20_000, |rng| {
        check_one(rng, RUNTIME_ALPHABETS)
    });
}

#[test]
fn every_string_carries_its_unit_count() {
    run("unit counts", WELL_FORMED_ALPHABETS, 20_000, |rng| {
        // Off ASCII the results differ from the model until phase 2 (byte positions); what is
        // checked here is that every input and result decodes to as many units as it stores
        // (`units` and `to_rt` panic otherwise).
        let _ = check_one(rng, WELL_FORMED_ALPHABETS);
        Ok(())
    });
}

#[test]
fn wtf8_round_trips_every_alphabet() {
    run("WTF-8 round trip", ALL_ALPHABETS, 5_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let back = wtf8_decode(&wtf8_encode(&s));
        if back == s {
            Ok(())
        } else {
            Err(format!("{} came back as {}", show(&s), show(&back)))
        }
    });
}

#[test]
fn model_is_consistent_on_every_alphabet() {
    run("model consistency", ALL_ALPHABETS, 5_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let n = gen_needle(rng, &s, ALL_ALPHABETS);
        let m = format!("{} / {}", show(&s), show(&n));
        // A found position is where the needle is.
        let k = model::index_of(&s, &n, 0);
        if k >= 0 && model::slice(&s, k, k + n.len() as i64) != n {
            return Err(format!("indexOf points elsewhere: {m}"));
        }
        // split then join with the separator gives the string back (for a non-empty separator).
        if !n.is_empty() && model::split(&s, &n).join(&n[..]) != s {
            return Err(format!("split/join: {m}"));
        }
        // replaceAll(n, n) changes nothing when `n` has no `$`.
        if !n.contains(&(b'$' as u16)) && model::replace_all(&s, &n, &n) != s {
            return Err(format!("replaceAll(n, n): {m}"));
        }
        // Ordering is antisymmetric.
        let t = mutate(rng, &s, ALL_ALPHABETS);
        if model::cmp(&s, &t) != -model::cmp(&t, &s) {
            return Err(format!("cmp not antisymmetric: {} {}", show(&s), show(&t)));
        }
        Ok(())
    });
}
