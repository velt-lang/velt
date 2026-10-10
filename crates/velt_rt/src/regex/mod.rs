//! `std/regex`: JavaScript-flavoured regular expressions over the Rust `regex` crate
//! (linear-time matching, no backtracking blowups).
//!
//! A compiled regex is an opaque `Arc` handle (`VeltRegex*`), so clones are cheap and a handle
//! can be shared by concurrent tasks. Matching runs on the WTF-8 bytes of the subject; the
//! offsets the functions take and return are UTF-16 code units, like every Velt string position
//! (#377 phase 2b), translated at entry and exit ([`Positions`]). In Unicode mode the engine
//! never matches the bytes of a lone surrogate (#377: `.` and negated classes accepting them is
//! phase 5), and its empty-match stepping skips one as a whole 3-byte sequence and a pair as a
//! whole (JavaScript's non-`u` stepping is #401). The `g`/`y` flags are iteration modes that std/regex
//! implements; the runtime only needs the others.

mod matches;
mod replace;
mod syntax;

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
}

/// Opaque handle (`Arc<RegexObj>`).
pub type RegexHandle = Handle<RegexObj>;

/// Compile `pattern` with JS `flags` (`d g i m s u v y`; `g`, `y`, `d`, `u`, `v` do not change
/// the pattern itself).
fn compile(pattern: &str, flags: &str) -> Result<RegexObj, String> {
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
    RegexBuilder::new(&syntax::translate(pattern, dot_all))
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
        })
        .map_err(|e| format!("Invalid regular expression: /{pattern}/{flags}: {e}"))
}

unsafe fn text<'a>(s: *const VeltStr) -> &'a [u8] {
    (*s).as_bytes()
}

/// The search start for code unit `from` of `s`, as a byte offset: a position inside a pair moves
/// to the pair's end (the engine matches code points, never half of one).
///
/// # Safety
/// `s` must be valid.
unsafe fn start_byte(s: &VeltStr, from: u64) -> usize {
    start_pos(s, from).0
}

/// [`start_byte`] with the code unit it stands for (`from` clamped to the length, or the unit
/// after the pair `from` was inside).
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

/// Byte offsets of matches in `s` as code units: offsets inside a match are counted from its
/// start, and each match start from the previous one (matches come in order), so translating
/// every match of a string scans it once.
struct Positions<'a> {
    s: &'a VeltStr,
    byte: usize,
    unit: usize,
}

impl<'a> Positions<'a> {
    /// Positions of matches at or after byte `byte` of `s`, which is code unit `unit`.
    fn new(s: &'a VeltStr, byte: usize, unit: usize) -> Positions<'a> {
        Positions { s, byte, unit }
    }

    /// The code unit of the start of a match at byte `b` (not before the previous match start).
    ///
    /// # Safety
    /// `self.s` must be valid and `b` a code point boundary.
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
        if self.s.is_ascii() {
            b
        } else if b >= self.byte {
            self.unit + crate::str::wtf8::count_units(&self.s.as_bytes()[self.byte..b])
        } else {
            self.s.byte_to_unit(b)
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
    let pattern = (*pattern).text_lossy();
    let flags = (*flags).text_lossy();
    let r = match compile(&pattern, &flags) {
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
    let from = start_byte(&*s, from);
    re.obj().re.find_at(text(s), from).is_some() as u8
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
    let (byte, unit) = start_pos(st, from);
    let Some(caps) = re.obj().re.captures_at(text(s), byte) else {
        return 0;
    };
    let mut v = Vec::with_capacity(2 * caps.len());
    let groups = caps.iter().map(|g| g.map(|m| (m.start(), m.end())));
    Positions::new(st, byte, unit).push(groups, &mut v);
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
    let re = &re.obj().re;
    let mut pos = Positions::new(&*s, 0, 0);
    let mut v = Vec::new();
    if re.captures_len() == 1 {
        matches::each_find(re, text(s), |start, end| {
            pos.push(std::iter::once(Some((start, end))), &mut v);
            true
        });
    } else {
        matches::each_captures(re, text(s), |locs| {
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
    let r = replace::replace(&re.obj().re, text(s), text(replacement), all != 0);
    out.write(VeltStr::from_vec(r));
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
    let parts = replace::split(&re.obj().re, text(s), limit);
    out.write(VeltStrArray::from_vec(
        parts.into_iter().map(VeltStr::from_vec).collect(),
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
