//! `std/regex`: JavaScript-flavoured regular expressions over the Rust `regex` crate
//! (linear-time matching, no backtracking blowups).
//!
//! A compiled regex is an opaque `Arc` handle (`VeltRegex*`), so clones are cheap and a handle
//! can be shared by concurrent tasks. Matching runs on the bytes of the (always valid UTF-8)
//! subject, so every offset is a byte offset on a character boundary — the same indexing model
//! as Velt strings (`slice`, `indexOf`). The `g`/`y` flags are iteration modes that std/regex
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
use std::sync::Arc;

/// A compiled pattern plus its capture-group names.
pub struct RegexObj {
    re: Regex,
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
        .map(|re| RegexObj { re })
        .map_err(|e| format!("Invalid regular expression: /{pattern}/{flags}: {e}"))
}

unsafe fn text<'a>(s: *const VeltStr) -> &'a [u8] {
    (*s).as_bytes()
}

/// `new RegExp(pattern, flags)` → `IoResult<VeltRegex*>`; a bad pattern or flag is `EINVAL`
/// with a JS-style message.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_new(
    pattern: *const VeltStr,
    flags: *const VeltStr,
    out: *mut IoResult<RegexHandle>,
) {
    let pattern = String::from_utf8_lossy(text(pattern));
    let flags = String::from_utf8_lossy(text(flags));
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

/// Whether the subject has a match starting at or after byte `from`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_test(re: RegexHandle, s: *const VeltStr, from: u64) -> u8 {
    let s = text(s);
    let from = (from as usize).min(s.len());
    re.obj().re.find_at(s, from).is_some() as u8
}

/// Group offsets of one match as `start, end` pairs (`-1, -1` for groups that did not take part).
fn push_offsets(caps: &regex::bytes::Captures<'_>, out: &mut Vec<i64>) {
    for g in caps.iter() {
        match g {
            Some(m) => out.extend([m.start() as i64, m.end() as i64]),
            None => out.extend([-1, -1]),
        }
    }
}

/// First match starting at or after byte `from`: returns 1 and writes `2 * group_count` offsets
/// (`i64[]`: start/end per group, `-1` for unmatched groups); 0 if there is none (`out`
/// untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_exec(
    re: RegexHandle,
    s: *const VeltStr,
    from: u64,
    out: *mut VeltArray<i64>,
) -> u8 {
    let s = text(s);
    let from = (from as usize).min(s.len());
    let Some(caps) = re.obj().re.captures_at(s, from) else {
        return 0;
    };
    let mut v = Vec::with_capacity(2 * caps.len());
    push_offsets(&caps, &mut v);
    out.write(VeltArray::from_vec(v));
    1
}

/// Every non-overlapping match (JS `matchAll`: an empty match advances by one character), as
/// consecutive groups of `2 * group_count` offsets.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_regex_exec_all(
    re: RegexHandle,
    s: *const VeltStr,
    out: *mut VeltArray<i64>,
) {
    let re = &re.obj().re;
    let s = text(s);
    let mut v = Vec::new();
    if re.captures_len() == 1 {
        matches::each_find(re, s, |start, end| {
            v.extend([start as i64, end as i64]);
            true
        });
    } else {
        matches::each_captures(re, s, |locs| {
            for g in 0..locs.len() {
                match locs.get(g) {
                    Some((a, b)) => v.extend([a as i64, b as i64]),
                    None => v.extend([-1, -1]),
                }
            }
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
    let s = String::from_utf8_lossy(text(s));
    out.write(VeltStr::from_vec(regex::escape(&s).into_bytes()));
}

#[cfg(test)]
mod tests;
