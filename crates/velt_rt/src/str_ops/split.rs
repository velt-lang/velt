//! `s.split(sep)`.

use super::{sub_string, text};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// `s.split(sep)` with JS semantics: an empty `sep` splits into characters (Unicode scalar
/// values, POC), `"".split("")` is `[]`, `"".split(",")` is `[""]`, adjacent/trailing
/// separators give empty strings. Pieces of a static string borrow from it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_split(
    s: *const VeltStr,
    sep: *const VeltStr,
    out: *mut VeltStrArray,
) {
    let (t, sep_text) = (text(s), text(sep));
    let pieces: Vec<VeltStr> = if sep_text.is_empty() {
        t.char_indices()
            .map(|(i, c)| sub_string(s, i, i + c.len_utf8()))
            .collect()
    } else {
        let mut pieces = Vec::new();
        let mut start = 0;
        // A byte search finds the same matches as a `str` one (UTF-8 is self-synchronizing),
        // and memchr's is vectorized.
        for i in memchr::memmem::find_iter(t.as_bytes(), sep_text.as_bytes()) {
            pieces.push(sub_string(s, start, i));
            start = i + sep_text.len();
        }
        pieces.push(sub_string(s, start, t.len()));
        pieces
    };
    out.write(VeltStrArray::from_vec(pieces));
}
