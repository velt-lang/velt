//! ABI-level tests of `velt_rt_regex_*` (expected values checked against node).

use super::*;
use std::mem::MaybeUninit;

fn s(text: &'static str) -> VeltStr {
    VeltStr::from_static(text.as_bytes())
}

fn new(pattern: &'static str, flags: &'static str) -> Result<RegexHandle, String> {
    let mut out = MaybeUninit::<IoResult<RegexHandle>>::uninit();
    // SAFETY: valid arguments; the result is read according to its code.
    unsafe {
        velt_rt_regex_new(&s(pattern), &s(flags), out.as_mut_ptr());
        let r = out.assume_init();
        if r.err.code == 0 {
            Ok(r.value.assume_init())
        } else {
            Err(take(r.err.message))
        }
    }
}

unsafe fn take(v: VeltStr) -> String {
    let t = String::from_utf8_lossy(v.as_bytes()).into_owned();
    let mut v = v;
    crate::str::velt_rt_str_drop(&mut v);
    t
}

fn replace(
    p: &'static str,
    flags: &'static str,
    subject: &'static str,
    rep: &'static str,
) -> String {
    let re = new(p, flags).expect("valid pattern");
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid handle and strings.
    unsafe {
        velt_rt_regex_replace(
            re,
            &s(subject),
            &s(rep),
            flags.contains('g') as u8,
            out.as_mut_ptr(),
        );
        velt_rt_regex_free(re);
        take(out.assume_init())
    }
}

fn split(p: &'static str, subject: &'static str, limit: u64) -> Vec<String> {
    let re = new(p, "").expect("valid pattern");
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid handle and strings; the array is dropped after copying.
    unsafe {
        velt_rt_regex_split(re, &s(subject), limit, out.as_mut_ptr());
        velt_rt_regex_free(re);
        let mut arr = out.assume_init();
        let v = std::slice::from_raw_parts(arr.ptr, arr.len as usize)
            .iter()
            .map(|x| String::from_utf8_lossy(x.as_bytes()).into_owned())
            .collect();
        crate::str_array::velt_rt_str_array_drop(&mut arr);
        v
    }
}

#[test]
fn exec_reports_group_offsets() {
    let re = new(r"(?<y>\d{4})-(\d\d)(x)?", "").unwrap();
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid handle and strings.
    unsafe {
        assert_eq!(velt_rt_regex_group_count(re), 4);
        assert_eq!(
            velt_rt_regex_exec(re, &s("on 2024-02!"), 0, out.as_mut_ptr()),
            1
        );
        let a = out.assume_init();
        assert_eq!(a.as_slice(), &[3, 10, 3, 7, 8, 10, -1, -1]);
        drop(Vec::from_raw_parts(a.ptr, a.len as usize, a.cap as usize));
        let mut none = MaybeUninit::uninit();
        assert_eq!(
            velt_rt_regex_exec(re, &s("on 2024-02!"), 4, none.as_mut_ptr()),
            0
        );
        assert_eq!(velt_rt_regex_test(re, &s("1999-12"), 0), 1);
        assert_eq!(velt_rt_regex_test(re, &s("1999-12"), 99), 0);
        velt_rt_regex_free(re);
    }
}

#[test]
fn flags_and_errors() {
    assert!(new("abc", "gimsuy").is_ok());
    let e = new("abc", "gg").unwrap_err();
    assert_eq!(e, "Invalid flags supplied to RegExp constructor 'gg'");
    assert!(new("(a", "")
        .unwrap_err()
        .starts_with("Invalid regular expression: /(a/: "));
    assert!(new(r"(?=a)", "").is_err(), "lookahead is not supported");
    // `\d` and `\w` are ASCII-only, as in JS.
    let re = new(r"^\w+$", "i").unwrap();
    // SAFETY: valid handle and strings.
    unsafe {
        assert_eq!(velt_rt_regex_test(re, &s("Hello_42"), 0), 1);
        assert_eq!(velt_rt_regex_test(re, &s("héllo"), 0), 0);
        velt_rt_regex_free(re);
    }
}

#[test]
fn replacement_patterns_follow_js() {
    assert_eq!(replace("(a)(b)?", "g", "xaby a", "[$2$1]"), "x[ba]y [a]");
    assert_eq!(replace("b", "", "abc", "$$-$&-$`-$'"), "a$-b-a-cc");
    assert_eq!(replace("(b)", "", "abc", "$1a$01$10$0"), "ababb0$0c");
    assert_eq!(replace("(?<w>o+)", "g", "foo boo", "<$<w>>"), "f<oo> b<oo>");
    assert_eq!(replace("o", "", "foo", "$<w>"), "f$<w>o");
    assert_eq!(replace("", "g", "ab", "-"), "-a-b-");
    assert_eq!(replace("x*", "g", "abc", "-"), "-a-b-c-");
}

#[test]
fn split_follows_js() {
    assert_eq!(split(",", "a,b,", 0), ["a", "b", ""]);
    assert_eq!(split(r"\s*(,)\s*", "a , b", 0), ["a", ",", "b"]);
    assert_eq!(split("", "abc", 0), ["a", "b", "c"]);
    assert_eq!(split("x*", "axxb", 0), ["a", "b"]);
    assert_eq!(split(",", "", 0), [""]);
    assert!(split("", "", 0).is_empty());
    assert_eq!(split(",", "a,b,c", 2), ["a", "b"]);
    assert_eq!(
        split("(-)|(\\+)", "1-2+3", 0),
        ["1", "-", "", "2", "", "+", "3"]
    );
}

#[test]
fn exec_all_and_escape() {
    let re = new("a(\\d)?", "g").unwrap();
    let mut out = MaybeUninit::uninit();
    let mut esc = MaybeUninit::uninit();
    // SAFETY: valid handle and strings.
    unsafe {
        velt_rt_regex_exec_all(re, &s("a1 a a2"), out.as_mut_ptr());
        let a = out.assume_init();
        assert_eq!(a.as_slice(), &[0, 2, 1, 2, 3, 4, -1, -1, 5, 7, 6, 7]);
        drop(Vec::from_raw_parts(a.ptr, a.len as usize, a.cap as usize));
        velt_rt_regex_free(re);
        velt_rt_regex_escape(&s("a.b*c"), esc.as_mut_ptr());
        assert_eq!(take(esc.assume_init()), r"a\.b\*c");
    }
}

/// U+2028/U+2029: `.` excludes them as in JS, but multiline `^`/`$` do not stop at them (JS:
/// `">a\u2028>b"`), the documented difference in docs/std/regex.md.
#[test]
fn line_separators() {
    assert_eq!(replace(".", "g", "a\u{2028}b", "-"), "-\u{2028}-");
    assert_eq!(replace("^", "gm", "a\nb\u{2029}c", ">"), ">a\n>b\u{2029}c");
}

#[test]
fn replacement_joins_halves_of_a_pair_and_matching_skips_lone_surrogates() {
    // WTF-8 of a lone high (U+D83D) and low (U+DE00) surrogate.
    let (hi, lo) = ([0xED, 0xA0, 0xBD], [0xED, 0xB8, 0x80]);
    let wtf = |parts: &[&[u8]]| VeltStr::from_bytes(&parts.concat());
    let re = new("-", "").expect("valid pattern");
    let run = |subject: &VeltStr, rep: &str, all: bool| unsafe {
        let mut out = MaybeUninit::uninit();
        velt_rt_regex_replace(
            re,
            subject,
            &s(Box::leak(rep.into())),
            all as u8,
            out.as_mut_ptr(),
        );
        let mut v = out.assume_init();
        let b = v.as_bytes().to_vec();
        crate::str::velt_rt_str_drop(&mut v);
        b
    };
    // The text before the match against the text after it, and `$&` pieces.
    assert_eq!(run(&wtf(&[&hi, b"-", &lo]), "", false), "😀".as_bytes());
    assert_eq!(
        run(&wtf(&[&hi, b"-", &lo, b"-"]), "$&", true),
        [&hi[..], b"-", &lo, b"-"].concat()
    );
    // `.` and `[^…]` don't match a lone surrogate yet (#377 phase 5), and an empty match steps
    // over one as a whole. Offsets are code units.
    let dot = new(".", "g").expect("valid pattern");
    let mut out = MaybeUninit::uninit();
    unsafe { velt_rt_regex_exec_all(dot, &wtf(&[b"a", &hi, b"b"]), out.as_mut_ptr()) };
    let found = unsafe { out.assume_init() };
    assert_eq!(unsafe { found.as_slice() }, [0, 1, 2, 3]);
    unsafe {
        velt_rt_regex_free(re);
        velt_rt_regex_free(dot);
    }
}

/// Every match of `x` in `subject` by an `exec(s, from)` loop, and the code units the loop
/// scanned to translate positions.
unsafe fn exec_loop(subject: &VeltStr) -> (Vec<i64>, usize) {
    let re = new("x", "").expect("valid pattern");
    let (mut starts, mut from) = (Vec::new(), 0);
    let before = crate::str::work::total();
    loop {
        let mut out = MaybeUninit::<VeltArray<i64>>::uninit();
        if velt_rt_regex_exec(re, subject, from, out.as_mut_ptr()) == 0 {
            break;
        }
        let a = out.assume_init();
        starts.push(a.as_slice()[0]);
        from = a.as_slice()[1] as u64;
        drop(Vec::from_raw_parts(a.ptr, a.len as usize, a.cap as usize));
    }
    let scanned = crate::str::work::total() - before;
    velt_rt_regex_free(re);
    (starts, scanned)
}

#[test]
fn exec_loops_over_non_ascii_subjects_stay_linear() {
    // Regression (#377 phase 2b review): each match start was counted from the start of the
    // subject, so the loop scanned O(n²) units. The work is counted, not timed.
    let mut text = String::new();
    let mut want = Vec::new();
    let mut units = 0;
    for i in 0..20_000 {
        if i % 7 == 0 {
            want.push(units as i64);
            text.push('x');
            units += 1;
        } else {
            text.push_str("é日");
            units += 2;
        }
    }
    let heap = VeltStr::from_bytes(text.as_bytes());
    let leaked: &'static [u8] = Box::leak(text.into_bytes().into_boxed_slice());
    let lit = VeltStr::from_static(leaked);
    for (form, subject) in [("heap", &heap), ("static", &lit)] {
        // SAFETY: valid strings.
        let scanned = crate::str::work::least_work(|| {
            let (starts, scanned) = unsafe { exec_loop(subject) };
            assert_eq!(starts, want);
            scanned
        });
        // About one unit per unit of the subject; the quadratic loop scanned ~10^8.
        assert!(scanned <= 4 * units, "{form}: {scanned} units scanned");
    }
    let mut heap = heap;
    // SAFETY: the test's own string.
    unsafe { heap.release() };
}
