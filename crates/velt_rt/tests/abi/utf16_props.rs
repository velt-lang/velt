//! The string operations through the C ABI, checked against the UTF-16 reference model
//! (utf16_model.rs, #377) on random inputs: random strings in every form (static, inline, heap),
//! with lengths around the inline limit (22, 23 bytes) and the breadcrumb stride (64, 128 units),
//! and random operations on them.
//!
//! The runtime counts UTF-8 bytes until phase 2b of #377, where only ASCII agrees with the model,
//! so the runtime's positions are checked on [`RUNTIME_ALPHABETS`]. Phase 2b adds the other
//! alphabets; the model checks below already run on all of them. Every runtime string the checks
//! touch must also carry its model length as its unit count and be canonical WTF-8 (phase 1), on
//! every alphabet, lone surrogates included (phase 2a); the operations whose result doesn't depend
//! on positions (concatenation, builders, `repeat`, equality, hashing), the position translation
//! of phase 2b ([`VeltStr::unit_to_byte`], [`VeltStr::byte_to_unit`]) and the code-unit order
//! ([`cmp_utf16`]) already agree with the model on every alphabet.
//!
//! `VELT_UTF16_SEED` replays a failing run (the failure message prints the seed);
//! `VELT_UTF16_CASES` changes the number of operations.

use super::utf16_model::{self as model, show, wtf8_decode, wtf8_encode};
use crate::hash::velt_rt_str_hash;
use crate::str::{
    cmp_utf16, velt_rt_str_cmp, velt_rt_str_concat, velt_rt_str_drop, wtf8, BytePos, VeltStr,
};
use crate::str_array::{velt_rt_str_array_drop, VeltStrArray};
use crate::str_ops::replace::*;
use crate::str_ops::search::*;
use crate::str_ops::slice::*;
use crate::str_ops::split::*;
use crate::strbuf::{velt_rt_strbuf_new, velt_rt_strbuf_push_str};
use std::mem::MaybeUninit;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Alphabet {
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

pub(super) const ALL_ALPHABETS: &[Alphabet] = &[
    Alphabet::Ascii,
    Alphabet::Bmp,
    Alphabet::Astral,
    Alphabet::Lone,
];

/// The alphabets on which the runtime agrees with the model. Phase 2 of #377 makes this
/// `ALL_ALPHABETS`.
const RUNTIME_ALPHABETS: &[Alphabet] = &[Alphabet::Ascii];

/// Lengths in code units around the inline limit and the breadcrumb stride; other lengths are
/// random up to 200.
const LENGTHS: &[usize] = &[0, 1, 2, 3, 21, 22, 23, 24, 63, 64, 65, 127, 128, 129];

/// xorshift64*: small, deterministic and good enough to pick test inputs.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    pub(super) fn chance(&mut self, percent: u64) -> bool {
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
pub(super) fn gen_string(rng: &mut Rng, alphabets: &[Alphabet]) -> Vec<u16> {
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
pub(super) struct Rt(pub(super) VeltStr);

impl Drop for Rt {
    fn drop(&mut self) {
        unsafe { velt_rt_str_drop(&mut self.0) };
    }
}

/// The model string `s` as a runtime string, in a random form.
pub(super) fn to_rt(rng: &mut Rng, s: &[u16]) -> Rt {
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

/// The code units of a runtime string; panics unless its bytes are canonical WTF-8, the unit
/// count stored in the value is their number and the string knows whether it is well-formed.
pub(super) fn units(s: &VeltStr) -> Vec<u16> {
    let bytes = unsafe { s.as_bytes() };
    if let Err(e) = wtf8::check_canonical(bytes) {
        panic!("{s:?} is not canonical WTF-8: {e}");
    }
    let u = wtf8_decode(bytes);
    assert_eq!(
        s.units(),
        u.len(),
        "stored unit count of {} ({s:?})",
        show(&u)
    );
    let lone = (0..u.len()).any(|i| {
        let (hi, lo) = (model::is_high(u[i]), model::is_low(u[i]));
        (hi && !u.get(i + 1).is_some_and(|&n| model::is_low(n)))
            || (lo && !(i > 0 && model::is_high(u[i - 1])))
    });
    assert_eq!(
        unsafe { s.is_well_formed() },
        !lone,
        "well-formedness of {}",
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

pub(super) fn run(
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
    run("unit counts", ALL_ALPHABETS, 20_000, |rng| {
        // Off ASCII the results differ from the model until phase 2b (byte positions); what is
        // checked here is that every input and result, also from lone surrogates, is canonical
        // and decodes to as many units as it stores (`units` and `to_rt` panic otherwise).
        let _ = check_one(rng, ALL_ALPHABETS);
        let _ = check_text_ops(rng, ALL_ALPHABETS);
        Ok(())
    });
}

#[test]
fn concatenation_and_repeat_match_the_model_on_every_alphabet() {
    run("exact ops", ALL_ALPHABETS, 20_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let n = gen_needle(rng, &s, ALL_ALPHABETS);
        let (rs, rn) = (to_rt(rng, &s), to_rt(rng, &n));
        let m = format!("{} / {}", show(&s), show(&n));
        unsafe {
            let got = out_str(|o| velt_rt_str_concat(&rs.0, &rn.0, o));
            if got != [s.clone(), n.clone()].concat() {
                return Err(format!("concat {m}: {}", show(&got)));
            }
            let mut buf = MaybeUninit::<VeltStr>::uninit();
            velt_rt_strbuf_new(rng.below(40) as u64, buf.as_mut_ptr());
            let mut buf = Rt(buf.assume_init());
            for p in [&s, &n, &s] {
                let rp = to_rt(rng, p);
                velt_rt_strbuf_push_str(&mut buf.0, &rp.0);
            }
            if units(&buf.0) != [s.clone(), n.clone(), s.clone()].concat() {
                return Err(format!("builder {m}"));
            }
            let k = rng.below(4) as i64;
            let mut out = MaybeUninit::<VeltStr>::uninit();
            velt_rt_str_repeat(&rs.0, k, out.as_mut_ptr());
            let got = units(&Rt(out.assume_init()).0);
            if Some(got) != model::repeat(&s, k) {
                return Err(format!("{}.repeat({k})", show(&s)));
            }
        }
        Ok(())
    });
}

/// The operations that build text without positions in the model's sense (case mapping,
/// trimming, JSON escaping): every result is checked for canonical form by [`units`], and
/// `JSON.stringify` against a model of it.
fn check_text_ops(rng: &mut Rng, alphabets: &[Alphabet]) -> Result<(), String> {
    use crate::str_ops::case::*;
    use crate::strbuf::velt_rt_strbuf_push_json_str;
    let s = gen_string(rng, alphabets);
    let rs = to_rt(rng, &s);
    unsafe {
        units(&Rt(out_str_rt(|o| velt_rt_str_to_upper(&rs.0, o))).0);
        units(&Rt(out_str_rt(|o| velt_rt_str_to_lower(&rs.0, o))).0);
        units(&Rt(out_str_rt(|o| velt_rt_str_trim(&rs.0, o))).0);
        let mut buf = MaybeUninit::<VeltStr>::uninit();
        velt_rt_strbuf_new(rng.below(40) as u64, buf.as_mut_ptr());
        let mut buf = Rt(buf.assume_init());
        velt_rt_strbuf_push_json_str(&mut buf.0, &rs.0);
        let got = units(&buf.0);
        let want = json_stringify(&s);
        if got != want {
            return Err(format!(
                "JSON.stringify({}): runtime {}, model {}",
                show(&s),
                show(&got),
                show(&want)
            ));
        }
    }
    Ok(())
}

fn out_str_rt(f: impl FnOnce(*mut VeltStr)) -> VeltStr {
    let mut out = MaybeUninit::<VeltStr>::uninit();
    f(out.as_mut_ptr());
    unsafe { out.assume_init() }
}

/// `JSON.stringify(s)` of the model (ES2019 well-formed: lone surrogates as `\udxxx`).
fn json_stringify(s: &[u16]) -> Vec<u16> {
    let mut out = vec![b'"' as u16];
    let push = |out: &mut Vec<u16>, t: &str| out.extend(t.bytes().map(u16::from));
    for (i, &u) in s.iter().enumerate() {
        let paired = (model::is_high(u) && s.get(i + 1).is_some_and(|&n| model::is_low(n)))
            || (model::is_low(u) && i > 0 && model::is_high(s[i - 1]));
        match u {
            0x22 => push(&mut out, "\\\""),
            0x5C => push(&mut out, "\\\\"),
            0x08 => push(&mut out, "\\b"),
            0x0C => push(&mut out, "\\f"),
            0x0A => push(&mut out, "\\n"),
            0x0D => push(&mut out, "\\r"),
            0x09 => push(&mut out, "\\t"),
            0..=0x1F => push(&mut out, &format!("\\u{u:04x}")),
            0xD800..=0xDFFF if !paired => push(&mut out, &format!("\\u{u:04x}")),
            _ => out.push(u),
        }
    }
    out.push(b'"' as u16);
    out
}

/// The byte position of unit `u` in the model string `s` (see [`BytePos`]).
fn model_byte_pos(s: &[u16], u: usize) -> BytePos {
    let inside = u > 0 && u < s.len() && model::is_high(s[u - 1]) && model::is_low(s[u]);
    let start = if inside { u - 1 } else { u };
    BytePos {
        byte: wtf8_encode(&s[..start]).len(),
        low_half: inside,
    }
}

#[test]
fn position_translation_matches_the_model() {
    run("position translation", ALL_ALPHABETS, 3_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let r = to_rt(rng, &s);
        // Sometimes shared, so a table is published for other readers (compare-and-swap).
        let other = rng.chance(30).then(|| Rt(unsafe { r.0.share() }));
        for u in 0..=s.len() + 1 {
            let want = model_byte_pos(&s, u.min(s.len()));
            let got = unsafe { r.0.unit_to_byte(u) };
            if got != want {
                return Err(format!("{} unit {u}: {got:?}, model {want:?}", show(&s)));
            }
            if !want.low_half {
                let back = unsafe { r.0.byte_to_unit(want.byte) };
                if back != u.min(s.len()) {
                    return Err(format!("{} byte {}: unit {back}", show(&s), want.byte));
                }
            }
        }
        drop(other);
        Ok(())
    });
}

#[test]
fn appended_strings_extend_their_breadcrumbs() {
    run("breadcrumbs after appends", ALL_ALPHABETS, 300, |rng| {
        let mut s: Vec<u16> = Vec::new();
        let mut r = Rt(VeltStr::empty());
        for _ in 0..1 + rng.below(6) {
            let piece = gen_string(rng, ALL_ALPHABETS);
            let rp = to_rt(rng, &piece);
            unsafe { crate::str::velt_rt_str_append(&mut r.0, &rp.0) };
            s.extend_from_slice(&piece);
            // Shared between appends now and then: the next append copies, and a translation
            // of the shared value publishes a longer table.
            let keep = rng.chance(20).then(|| Rt(unsafe { r.0.share() }));
            for _ in 0..8 {
                let u = rng.below(s.len() + 1);
                let got = unsafe { r.0.unit_to_byte(u) };
                if got != model_byte_pos(&s, u) {
                    return Err(format!("{} unit {u}: {got:?}", show(&s)));
                }
            }
            drop(keep);
        }
        units(&r.0);
        Ok(())
    });
}

#[test]
fn utf16_order_matches_the_model() {
    run("code-unit order", ALL_ALPHABETS, 300_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let t = if rng.chance(50) {
            gen_string(rng, ALL_ALPHABETS)
        } else {
            mutate(rng, &s, ALL_ALPHABETS)
        };
        let (a, b) = (wtf8_encode(&s), wtf8_encode(&t));
        let got = cmp_utf16(&a, &b) as i32;
        let want = model::cmp(&s, &t);
        if got != want {
            return Err(format!("{} vs {}: {got}, model {want}", show(&s), show(&t)));
        }
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
