//! The text custom inspects wrote in a printed value (`console.log`, `${x}`), which
//! `velt_rt_strbuf_inspect_layout` keeps as one piece (`velt_rt_strbuf_inspect_atom`), per
//! top-level value. The cycles of a printed object graph (`<ref *N>`, `[Circular *N]`) are
//! std's (std/lang/hooks/inspect_cycles.velt).

use crate::strbuf::VeltStrBuf;
use std::ops::Range;

/// Text a class's custom inspect wrote (`velt_rt_strbuf_inspect_atom`): the builder's address
/// and the byte range, which `inspect_layout` keeps as one piece. Cleared at the start of each
/// top-level value, so a range never outlives the value it was written for.
struct Printing {
    atoms: Vec<(usize, Range<usize>)>,
}

thread_local! {
    static PRINTING: std::cell::RefCell<Printing> = const {
        std::cell::RefCell::new(Printing { atoms: Vec::new() })
    };
}

/// Start printing a top-level value (a `console.log` argument, a `${x}` or `String(x)`): the
/// pieces of the value before are forgotten. Compiled code calls it unless an object is being
/// printed (std's `inspectBegin`): a value printed from inside one keeps them.
#[no_mangle]
pub extern "C" fn velt_rt_strbuf_inspect_begin() {
    PRINTING.with(|s| s.borrow_mut().atoms.clear())
}

/// The text from byte `start` of the builder to its end is what a class's custom inspect
/// (`[INSPECT]()`) returned: `velt_rt_strbuf_inspect_layout` keeps it as one piece, as node
/// inserts the string a custom inspect returns without laying it out again.
///
/// # Safety
/// `buf` must be a valid builder and `start` at most its length.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_atom(buf: *const VeltStrBuf, start: u64) {
    let end = (*buf).len();
    PRINTING.with(|s| {
        s.borrow_mut()
            .atoms
            .push((buf as usize, start as usize..end));
    })
}

/// The pieces registered for the builder at `buf` (`velt_rt_strbuf_inspect_atom`) that lie in
/// the text from byte `start` on, relative to `start`, into `out` (cleared first).
pub(crate) fn atoms_of(buf: *const VeltStrBuf, start: usize, out: &mut Vec<Range<usize>>) {
    out.clear();
    PRINTING.with(|s| {
        let atoms = &s.borrow().atoms;
        for (b, r) in atoms {
            if *b == buf as usize && r.start >= start {
                out.push(r.start - start..r.end - start);
            }
        }
    })
}
