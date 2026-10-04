//! `s.split(sep)`.

use super::{bytes, sub_string};
use crate::str::wtf8;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// `s.split(sep)` with JS semantics: an empty `sep` splits into characters (code points, a lone
/// surrogate being one; POC), `"".split("")` is `[]`, `"".split(",")` is `[""]`,
/// adjacent/trailing separators give empty strings. Pieces of a static string borrow from it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_split(
    s: *const VeltStr,
    sep: *const VeltStr,
    out: *mut VeltStrArray,
) {
    let (t, sep_bytes) = (bytes(s), bytes(sep));
    let pieces: Vec<VeltStr> = if sep_bytes.is_empty() {
        let mut starts = wtf8::boundaries(t).peekable();
        let mut pieces = Vec::with_capacity(t.len().div_ceil(4));
        while let Some(i) = starts.next() {
            let end = starts.peek().copied().unwrap_or(t.len());
            pieces.push(sub_string(s, i, end));
        }
        pieces
    } else {
        let mut pieces = Vec::new();
        let mut start = 0;
        // A byte search finds the matches at code point boundaries (WTF-8 is
        // self-synchronizing), and memchr's is vectorized.
        for i in memchr::memmem::find_iter(t, sep_bytes) {
            pieces.push(sub_string(s, start, i));
            start = i + sep_bytes.len();
        }
        pieces.push(sub_string(s, start, t.len()));
        pieces
    };
    out.write(VeltStrArray::from_vec(pieces));
}
