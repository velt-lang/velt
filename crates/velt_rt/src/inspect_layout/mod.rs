//! `console.log`'s line breaking: node's `util.inspect` prints a container on one line only when
//! it fits in `breakLength` (80) columns, else one entry per line with 2-space indentation, and
//! an array of more than six short entries in aligned columns (`reduceToSingleString`,
//! `isBelowBreakLength` and `groupArrayElements` in node's `lib/internal/util/inspect.js`).
//!
//! The print glue writes a value on one line; [`velt_rt_strbuf_inspect_layout`] then re-reads
//! that text (`parse`) and lays it out again only when it may need breaking, so a short value
//! costs one length check. Node's `depth` and `maxArrayLength` limits are applied by the glue
//! as it writes the value; an array's `... n more items` entry stays out of its columns here.

mod group;
mod parse;
mod render;

use crate::strbuf::VeltStrBuf;
use parse::Value;
use render::{Render, Work};
use std::cell::RefCell;

/// Node's `breakLength`.
const BREAK_LENGTH: usize = 80;

/// A value whose one-line text is at most this long needs no breaking unless it has an array
/// of more than six entries: node keeps a container on one line while its text plus 9 columns
/// fits in [`BREAK_LENGTH`], and a string splits only when longer than 76.
const FITS: usize = 71;

/// Largest scratch buffer kept for the next printed value, in bytes.
const KEEP: usize = 1 << 16;

/// Re-lay out the text of one printed value, from byte `start` of the builder to its end, the
/// way node breaks it across lines (a `console.log` argument or a `${}` value).
///
/// # Safety
/// `buf` must be a valid builder and `start` at most its length, at the start of the value.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_layout(buf: *mut VeltStrBuf, start: u64) {
    let start = start as usize;
    let text = &(*buf).as_bytes()[start..];
    if text.len() <= FITS && text.iter().filter(|&&b| b == b',').count() < 6 {
        return;
    }
    SCRATCH.with(|scratch| {
        let Ok(mut scratch) = scratch.try_borrow_mut() else {
            return;
        };
        if scratch.layout(text) && scratch.out != text {
            (*buf).replace_tail(start, &scratch.out);
        }
        if scratch.out.capacity() > KEEP {
            // Not held on to after printing a huge value.
            *scratch = Scratch::default();
        }
    });
}

/// Byte length of the builder: where the next value's text starts.
///
/// # Safety
/// `buf` must be a valid builder.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_len(buf: *const VeltStrBuf) -> u64 {
    (*buf).len() as u64
}

/// Buffers reused from one printed value to the next.
#[derive(Default)]
struct Scratch {
    value: Value,
    work: Work,
    /// The laid-out text.
    out: Vec<u8>,
}

impl Scratch {
    /// Lay out the value printed on one line as `text` into `self.out`; false if it is not the
    /// glue's text.
    fn layout(&mut self, text: &[u8]) -> bool {
        if !self.value.parse(text) {
            return false;
        }
        self.out.clear();
        let r = Render {
            src: text,
            value: &self.value,
        };
        r.entry(self.value.top.clone(), 0, &mut self.work, &mut self.out);
        true
    }
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

/// The laid-out text of a value printed on one line, or `None` if it is not the glue's text.
#[cfg(test)]
fn layout(text: &[u8]) -> Option<Vec<u8>> {
    let mut s = Scratch::default();
    s.layout(text).then_some(s.out)
}

#[cfg(test)]
mod tests;
