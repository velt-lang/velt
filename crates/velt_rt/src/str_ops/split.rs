//! A string's characters (`for (const c of s)`); `s.split(sep)` is Velt code
//! (std/lang/string/split.velt).

use super::{bytes, sub_string};
use crate::str::wtf8;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// The characters of `s` (code points, a pair as one string, a lone surrogate as one), as JS's
/// string iterator yields them: `for (const c of s)`, `[...s]` and `Array.from(s)`. Pieces of a
/// static string borrow from it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_code_points(s: *const VeltStr, out: *mut VeltStrArray) {
    let t = bytes(s);
    let mut starts = wtf8::boundaries(t).peekable();
    let mut pieces = Vec::with_capacity((*s).units());
    while let Some(i) = starts.next() {
        let end = starts.peek().copied().unwrap_or(t.len());
        pieces.push(sub_string(s, i, end));
    }
    out.write(VeltStrArray::from_vec(pieces));
}
