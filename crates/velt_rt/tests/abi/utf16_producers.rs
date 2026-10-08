//! The producers of string values checked against a recount (#377): a wrong unit count makes a
//! heap buffer be freed with the wrong layout (a header exactly when units != bytes), and a wrong
//! lone-surrogate count makes `text()` hand ill-formed bytes to `from_utf8_unchecked`. Every
//! string here is checked by [`units`] (canonical WTF-8, the stored unit count equals the decoded
//! one, well-formedness as the model says) after each step, on every alphabet.
//!
//! - `push_wtf8` sequences, from every starting form, with the piece's summary given or counted,
//!   pieces that join the previous one (a high surrogate meeting a low one) included;
//! - the formatting producers that compute their output's counts by formula (`inspect` quoting,
//!   `JSON.stringify` escaping) through the builder.

use super::utf16_model::{show, wtf8_encode};
use super::utf16_props::{gen_string, run, to_rt, units, Rng, Rt, ALL_ALPHABETS};
use crate::str::{wtf8, VeltStr};
use crate::strbuf::{velt_rt_strbuf_push_inspect_str, velt_rt_strbuf_push_json_str};

/// A starting string in a random form: empty, a builder with room, static, inline or heap.
fn start(rng: &mut Rng, s: &[u16]) -> Rt {
    match rng.below(4) {
        0 if s.is_empty() => Rt(VeltStr::empty()),
        1 => {
            let mut b = Rt(VeltStr::with_capacity(rng.below(80)));
            unsafe { b.0.push_wtf8(&wtf8_encode(s), None) };
            b
        }
        _ => to_rt(rng, s),
    }
}

#[test]
fn push_wtf8_keeps_counts_and_layout() {
    run("push_wtf8 sequences", ALL_ALPHABETS, 4_000, |rng| {
        let mut model = gen_string(rng, ALL_ALPHABETS);
        model.truncate(rng.below(40));
        let mut r = start(rng, &model);
        for _ in 0..1 + rng.below(8) {
            let piece = gen_string(rng, ALL_ALPHABETS);
            let bytes = wtf8_encode(&piece);
            let summary = rng.chance(50).then(|| wtf8::summarize(&bytes));
            // Sometimes shared while pushing: the push must copy, the other copy stay intact.
            let keep = rng
                .chance(15)
                .then(|| (Rt(unsafe { r.0.share() }), model.clone()));
            unsafe { r.0.push_wtf8(&bytes, summary) };
            model.extend_from_slice(&piece);
            if units(&r.0) != model {
                return Err(format!("pushed {} onto {}", show(&piece), show(&model)));
            }
            if let Some((other, before)) = keep {
                if units(&other.0) != before {
                    return Err(format!("a shared copy changed: {}", show(&before)));
                }
            }
        }
        Ok(())
    });
}

#[test]
fn formatting_producers_count_what_they_write() {
    run("inspect / JSON escaping", ALL_ALPHABETS, 4_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let rs = to_rt(rng, &s);
        // Appended to a builder that may already hold text (a join at the opening quote is
        // impossible, but the counts add).
        let prefix = gen_string(rng, ALL_ALPHABETS);
        let mut b = start(rng, &prefix);
        unsafe {
            velt_rt_strbuf_push_inspect_str(&mut b.0, &rs.0);
            velt_rt_strbuf_push_json_str(&mut b.0, &rs.0);
        }
        // `units` checks the stored counts against the bytes.
        units(&b.0);
        Ok(())
    });
}

/// A JSON string literal of the units `s`, as source code units: each unit raw or as a `\uXXXX`
/// escape at random (quotes, backslashes and controls always escaped). Raw halves next to each
/// other become a pair when the source is encoded, as in a Velt string.
fn json_literal(rng: &mut Rng, s: &[u16]) -> Vec<u16> {
    let mut out = vec![b'"' as u16];
    for &u in s {
        if u < 0x20 || u == b'"' as u16 || u == b'\\' as u16 || rng.chance(40) {
            out.extend(format!("\\u{u:04x}").encode_utf16());
        } else {
            out.push(u);
        }
    }
    out.push(b'"' as u16);
    out
}

#[test]
fn json_strings_decode_to_their_code_units() {
    use crate::json::reader_abi::*;
    use crate::json::value_abi::*;
    use std::mem::MaybeUninit;
    run("JSON string decoding", ALL_ALPHABETS, 4_000, |rng| {
        let s = gen_string(rng, ALL_ALPHABETS);
        let src = json_literal(rng, &s);
        let rsrc = to_rt(rng, &src);
        unsafe {
            // The pull reader (`JSON.parse<string>`).
            let r = velt_rt_json_reader_new(&rsrc.0);
            let mut out = MaybeUninit::<VeltStr>::uninit();
            let ok = velt_rt_json_reader_read_string(r, out.as_mut_ptr()) == 1;
            velt_rt_json_reader_free(r);
            if !ok {
                return Err(format!("reader refused {}", show(&src)));
            }
            let got = units(&Rt(out.assume_init_read()).0);
            if got != s {
                return Err(format!("reader: {} gave {}", show(&src), show(&got)));
            }
            // The tree (`JSON.parseValue`).
            let (mut h, mut err) = (MaybeUninit::uninit(), MaybeUninit::<VeltStr>::uninit());
            if velt_rt_json_parse_value(&rsrc.0, h.as_mut_ptr(), err.as_mut_ptr()) != 1 {
                return Err(format!("parseValue refused {}", show(&src)));
            }
            let h = h.assume_init();
            let ok = velt_rt_json_value_as_str(h, out.as_mut_ptr()) == 1;
            let len = velt_rt_json_value_len(h);
            velt_rt_json_value_free(h);
            let got = units(&Rt(out.assume_init()).0);
            if !ok || got != s || len != s.len() as u64 {
                return Err(format!(
                    "parseValue: {} gave {} ({len})",
                    show(&src),
                    show(&got)
                ));
            }
        }
        Ok(())
    });
}
