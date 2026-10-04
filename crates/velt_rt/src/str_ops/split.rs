//! `s.split(sep)`.

use super::units::{self, is_half_needle};
use super::{bytes, sub_string};
use crate::str::wtf8;
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

/// `s.split(sep)` with JS semantics: an empty `sep` splits into code units (a pair into its two
/// halves, each a lone surrogate), `"".split("")` is `[]`, `"".split(",")` is `[""]`,
/// adjacent/trailing separators give empty strings. Pieces of a static string borrow from it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_split(
    s: *const VeltStr,
    sep: *const VeltStr,
    out: *mut VeltStrArray,
) {
    let (t, sep_bytes) = (bytes(s), bytes(sep));
    let pieces: Vec<VeltStr> = if sep_bytes.is_empty() {
        split_units(s, t)
    } else if is_half_needle(sep_bytes) && !(*s).is_ascii() {
        split_half(t, sep_bytes)
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

/// `split("")`: one string per code unit; a supplementary character gives its two halves.
unsafe fn split_units(s: *const VeltStr, t: &[u8]) -> Vec<VeltStr> {
    let mut starts = wtf8::boundaries(t).peekable();
    let mut pieces = Vec::with_capacity((*s).units());
    let mut buf = [0u8; 4];
    while let Some(i) = starts.next() {
        let end = starts.peek().copied().unwrap_or(t.len());
        if end - i == 4 {
            let cp = wtf8::decode_at(t, i).0;
            for half in [units::high_of(cp), units::low_of(cp)] {
                pieces.push(VeltStr::from_bytes(wtf8::encode(half as u32, &mut buf)));
            }
        } else {
            pieces.push(sub_string(s, i, end));
        }
    }
    pieces
}

/// `split(sep)` by a separator that can match half of a pair, in code units.
fn split_half(t: &[u8], sep: &[u8]) -> Vec<VeltStr> {
    let (hay, needle) = (units::decode(t), units::decode(sep));
    let mut pieces = Vec::new();
    let mut start = 0;
    for k in units::find_all(&hay, &needle, usize::MAX) {
        pieces.push(VeltStr::from_vec(units::encode(&hay[start..k])));
        start = k + needle.len();
    }
    pieces.push(VeltStr::from_vec(units::encode(&hay[start..])));
    pieces
}
