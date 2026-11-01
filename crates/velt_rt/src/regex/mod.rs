//! `std/regex`: JavaScript-flavoured regular expressions over the Rust `regex` crate
//! (linear-time matching, no backtracking blowups).
//!
//! A compiled regex is an opaque `Arc` handle (`VeltRegex*`), so clones are cheap and a handle
//! can be shared by concurrent tasks. Matching runs on the WTF-8 bytes of the subject; the
//! offsets the functions take and return are UTF-16 code units, like every Velt string position
//! (#377 phase 2b), translated at entry and exit ([`Positions`]). Unless the pattern names a
//! surrogate (surrogates.rs), the engine never matches the bytes of a lone surrogate (#377: `.`
//! and negated classes accepting them is phase 5), and its empty-match stepping skips one as a
//! whole 3-byte sequence and a pair as a whole (JavaScript's non-`u` stepping is #401). A
//! pattern without the `u` flag that names a surrogate searches the subject's units form
//! (units.rs), where each half of a pair is matched on its own, as in JS. The `g`/`y` flags are iteration modes that std/regex
//! implements; the runtime only needs the others.

mod matches;
mod replace;
mod surrogates;
mod syntax;
mod units;

use crate::array::VeltArray;
use crate::handle::Handle;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use regex::bytes::{Regex, RegexBuilder};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A compiled pattern, and JavaScript's `lastIndex` of the `RegExp` that owns the handle.
pub struct RegexObj {
    re: Regex,
    /// `lastIndex` (an `f64`'s bits). Kept here, not in a field of the std class, so that the
    /// methods that update it (`exec`, `s.replace(re, …)`) read their receiver as JS code
    /// expects, without a mutable borrow. Atomic because a handle may be shared between tasks.
    last_index: AtomicU64,
    /// The pattern matches code units: a subject with a pair is searched in its units form
    /// (units.rs).
    units: bool,
    /// The units form of the last subject searched (`units` patterns), so that a loop over
    /// one string converts it once.
    last: parking_lot::Mutex<Option<Arc<units::Form>>>,
}

/// Opaque handle (`Arc<RegexObj>`).
pub type RegexHandle = Handle<RegexObj>;

/// Compile `pattern` (WTF-8) with JS `flags` (`d g i m s u v y`; `g`, `y` and `d` do not change
/// the pattern itself).
fn compile(pattern: &[u8], flags: &str) -> Result<RegexObj, String> {
    let mut seen = String::new();
    for f in flags.chars() {
        if seen.contains(f) || !"dgimsuvy".contains(f) {
            return Err(format!(
                "Invalid flags supplied to RegExp constructor '{flags}'"
            ));
        }
        seen.push(f);
    }
    let multi_line = seen.contains('m');
    let dot_all = seen.contains('s');
    let unicode = seen.contains('u') || seen.contains('v');
    let invalid = |e: &dyn std::fmt::Display| {
        let pattern = crate::str::wtf8::to_utf8_lossy(pattern);
        format!("Invalid regular expression: /{pattern}/{flags}: {e}")
    };
    let js = surrogates::prepare(pattern, unicode);
    let t = syntax::translate(&js, dot_all, unicode).map_err(|e| invalid(&e))?;
    RegexBuilder::new(&t.pattern)
        .case_insensitive(seen.contains('i'))
        .dot_matches_new_line(dot_all)
        .multi_line(multi_line)
        // JS `^`/`$` also stop at `\r`; `.` is handled by the translation.
        .crlf(multi_line)
        .size_limit(1 << 24)
        .build()
        .map(|re| RegexObj {
            re,
            last_index: AtomicU64::new(0),
            units: t.surrogates && !unicode,
            last: parking_lot::Mutex::new(None),
        })
        .map_err(|e| invalid(&e))
}

/// The text the engine searches for a subject: its own bytes, or its units form.
enum Subject<'a> {
    Bytes(&'a [u8]),
    Units(Arc<units::Form>),
}

impl Subject<'_> {
    fn text(&self) -> &[u8] {
        match self {
            Subject::Bytes(b) => b,
            Subject::Units(f) => f.text(),
        }
    }

    fn is_units(&self) -> bool {
        matches!(self, Subject::Units(_))
    }
}

impl RegexObj {
    /// The units form of `s` (non-ASCII), from the cache when `s` was the last subject.
    ///
    /// # Safety
    /// `s` must be valid.
    unsafe fn form(&self, s: &VeltStr) -> Arc<units::Form> {
        // An inline string is short and has no fixed address: it is converted every time.
        if !(s.is_heap() || s.is_static()) {
            return Arc::new(units::Form::new(s, false));
        }
        let mut cache = self.last.lock();
        if let Some(f) = cache.as_ref().filter(|f| f.is_of(s)) {
            return f.clone();
        }
        let f = Arc::new(units::Form::new(s, true));
        *cache = Some(f.clone());
        f
    }

    /// The text the engine searches for subject `s`: its bytes, or its units form when the
    /// pattern matches code units and `s` holds a pair.
    ///
    /// # Safety
    /// `s` must be valid.
    unsafe fn subject<'a>(&self, s: &'a VeltStr) -> Subject<'a> {
        if !self.units || s.is_ascii() {
            return Subject::Bytes(s.as_bytes());
        }
        let form = self.form(s);
        match form.has_pairs() {
            true => Subject::Units(form),
            false => Subject::Bytes(s.as_bytes()),
        }
    }

    /// The text to search for a match of subject `s` at or after code unit `from`, the byte
    /// offset in it to start at, and the code unit that is: [`start_pos`] over the bytes of `s`,
    /// or the position of unit `from` in the units form (a pair's low half included).
    ///
    /// # Safety
    /// `s` must be valid.
    unsafe fn search_from<'a>(&self, s: &'a VeltStr, from: u64) -> (Subject<'a>, usize, usize) {
        let subject = self.subject(s);
        let Subject::Units(form) = &subject else {
            let (byte, unit) = start_pos(s, from);
            return (subject, byte, unit);
        };
        let unit = usize::try_from(from).unwrap_or(usize::MAX).min(s.units());
        let pos = s.unit_to_byte(unit);
        let byte = form.byte_of(pos.byte) + if pos.low_half { 3 } else { 0 };
        (subject, byte, unit)
    }
}

unsafe fn text<'a>(s: *const VeltStr) -> &'a [u8] {
    (*s).as_bytes()
}

/// Text taken out of the searched text, as a string's canonical WTF-8.
fn canonical(bytes: Vec<u8>, units_form: bool) -> Vec<u8> {
    match units_form {
        true => units::to_wtf8(&bytes),
        false => bytes,
    }
}

/// The search start for code unit `from` of `s`, as a byte offset, with the code unit it stands
/// for (`from` clamped to the length): a position inside a pair moves to the pair's end, the
/// unit after it (the engine matches code points, never half of one).
///
/// # Safety
/// `s` must be valid.
unsafe fn start_pos(s: &VeltStr, from: u64) -> (usize, usize) {
    let unit = usize::try_from(from).unwrap_or(usize::MAX).min(s.units());
    let pos = s.unit_to_byte(unit);
    match pos.low_half {
        true => (pos.byte + 4, unit + 1),
        false => (pos.byte, unit),
    }
}

/// Byte offsets of matches in the searched text as code units: offsets inside a match are
/// counted from its start, and each match start from the previous one (matches come in order),
/// so translating every match of a string scans it once.
struct Positions<'a> {
    text: &'a [u8],
    ascii: bool,
    byte: usize,
    unit: usize,
}

impl<'a> Positions<'a> {
    /// Positions of matches at or after byte `byte` of `text` (a subject's bytes or units form,
    /// `ascii` when it is all ASCII), which is code unit `unit`.
    fn new(text: &'a [u8], ascii: bool, byte: usize, unit: usize) -> Positions<'a> {
        Positions {
            text,
            ascii,
            byte,
            unit,
        }
    }

    /// The code unit of the start of a match at byte `b` (not before the previous match start).
    ///
    /// # Safety
    /// `b` must be a code point boundary of the text.
    unsafe fn start(&mut self, b: usize) -> i64 {
        self.unit = self.at(b);
        self.byte = b;
        self.unit as i64
    }

    /// The code unit of byte `b` of the current match (or -1 for a group that did not take part).
    ///
    /// # Safety
    /// As for [`Self::start`].
    unsafe fn at(&self, b: usize) -> usize {
        use crate::str::wtf8::count_units;
        if self.ascii {
            b
        } else if b >= self.byte {
            self.unit + count_units(&self.text[self.byte..b])
        } else {
            self.unit - count_units(&self.text[b..self.byte])
        }
    }

    /// The group offsets of a match (`start, end` per group, -1 for one that did not take part),
    /// group 0 first.
    ///
    /// # Safety
    /// As for [`Self::start`].
    unsafe fn push(
        &mut self,
        groups: impl Iterator<Item = Option<(usize, usize)>>,
        out: &mut Vec<i64>,
    ) {
        let mut first = true;
        for g in groups {
            match g {
                Some((a, b)) if first => {
                    let a = self.start(a);
                    out.extend([a, self.at(b) as i64]);
                }
                Some((a, b)) => out.extend([self.at(a) as i64, self.at(b) as i64]),
                None => out.extend([-1, -1]),
            }
            first = false;
        }
    }
}

/// `new RegExp(pattern, flags)` → `IoResult<VeltRegex*>`; a bad pattern or flag is `EINVAL`
/// with a JS-style message.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_new(
    pattern: *const VeltStr,
    flags: *const VeltStr,
    out: *mut IoResult<RegexHandle>,
) {
    let flags = (*flags).text_lossy();
    let r = match compile((*pattern).as_bytes(), &flags) {
        Ok(obj) => IoResult::ok(Handle::from_arc(Arc::new(obj))),
        Err(msg) => IoResult::err(VeltErr::new(code::INVALID_INPUT, &msg)),
    };
    out.write(r);
}

/// Release a handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_free(re: RegexHandle) {
    re.release();
}

/// The regex's `lastIndex` (0 for a new one).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_last_index(re: RegexHandle) -> f64 {
    f64::from_bits(re.obj().last_index.load(Ordering::Relaxed))
}

/// Sets the regex's `lastIndex`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_set_last_index(re: RegexHandle, value: f64) {
    re.obj()
        .last_index
        .store(value.to_bits(), Ordering::Relaxed);
}

/// Number of capture groups, including group 0 (the whole match).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_group_count(re: RegexHandle) -> u64 {
    re.obj().re.captures_len() as u64
}

/// Names of groups 1.. (`""` for unnamed groups), as an owned `string[]`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_group_names(re: RegexHandle, out: *mut VeltStrArray) {
    let names = re
        .obj()
        .re
        .capture_names()
        .skip(1)
        .map(|n| n.unwrap_or("").to_string());
    out.write(VeltStrArray::from_strings(names));
}

/// Whether the subject has a match starting at or after code unit `from`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_test(re: RegexHandle, s: *const VeltStr, from: u64) -> u8 {
    let (subject, byte, _) = re.obj().search_from(&*s, from);
    matches::find_at(&re.obj().re, subject.text(), byte).is_some() as u8
}

/// First match starting at or after code unit `from`: returns 1 and writes `2 * group_count`
/// offsets in code units (`i64[]`: start/end per group, `-1` for unmatched groups); 0 if there is
/// none (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_exec(
    re: RegexHandle,
    s: *const VeltStr,
    from: u64,
    out: *mut VeltArray<i64>,
) -> u8 {
    let st = &*s;
    // The search start's unit is known: offsets are counted from it, not from the start of the
    // subject (an `exec(s, from)` loop over a long non-ASCII subject stays linear).
    let (subject, byte, unit) = re.obj().search_from(st, from);
    let text = subject.text();
    let regex = &re.obj().re;
    let mut locs = regex.capture_locations();
    if matches::captures_at(regex, &mut locs, text, byte).is_none() {
        return 0;
    }
    let mut v = Vec::with_capacity(2 * locs.len());
    let groups = (0..locs.len()).map(|g| locs.get(g));
    Positions::new(text, st.is_ascii(), byte, unit).push(groups, &mut v);
    out.write(VeltArray::from_vec(v));
    1
}

/// Every non-overlapping match (JS `matchAll`: an empty match advances by one character), as
/// consecutive groups of `2 * group_count` offsets in code units.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_exec_all(
    re: RegexHandle,
    s: *const VeltStr,
    out: *mut VeltArray<i64>,
) {
    let subject = re.obj().subject(&*s);
    let text = subject.text();
    let re = &re.obj().re;
    let mut pos = Positions::new(text, (*s).is_ascii(), 0, 0);
    let mut v = Vec::new();
    if re.captures_len() == 1 {
        matches::each_find(re, text, |start, end| {
            pos.push(std::iter::once(Some((start, end))), &mut v);
            true
        });
    } else {
        matches::each_captures(re, text, |locs| {
            pos.push((0..locs.len()).map(|g| locs.get(g)), &mut v);
            true
        });
    }
    out.write(VeltArray::from_vec(v));
}

/// `s.replace(re, replacement)` (first match) or, with `all`, every match; `replacement`
/// expands JS patterns `$$ $& $` $' $n $nn $<name>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_replace(
    re: RegexHandle,
    s: *const VeltStr,
    replacement: *const VeltStr,
    all: u8,
    out: *mut VeltStr,
) {
    let subject = re.obj().subject(&*s);
    let r = replace::replace(&re.obj().re, subject.text(), text(replacement), all != 0);
    out.write(VeltStr::from_vec(canonical(r, subject.is_units())));
}

/// JS `s.split(re, limit)`: pieces between matches, with captured groups spliced in (`""` for
/// groups that did not take part); `limit == 0` means no limit.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_split(
    re: RegexHandle,
    s: *const VeltStr,
    limit: u64,
    out: *mut VeltStrArray,
) {
    let limit = if limit == 0 {
        usize::MAX
    } else {
        limit as usize
    };
    let subject = re.obj().subject(&*s);
    let parts = replace::split(&re.obj().re, subject.text(), limit);
    out.write(VeltStrArray::from_vec(
        parts
            .into_iter()
            .map(|p| VeltStr::from_vec(canonical(p, subject.is_units())))
            .collect(),
    ));
}

/// `RegExp.escape(s)`: `s` with every regex metacharacter escaped.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_escape(s: *const VeltStr, out: *mut VeltStr) {
    // Lone surrogates are no metacharacters: they stay as they are between escaped runs.
    let escaped = crate::str::wtf8::map_runs(text(s), |run, out| {
        out.extend_from_slice(regex::escape(run).as_bytes())
    });
    out.write(VeltStr::from_vec(escaped));
}

#[cfg(test)]
mod tests;
