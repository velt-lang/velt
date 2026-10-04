//! Cycles in printed object graphs (`console.log`, `${x}`): node prints a reference back to an
//! object being printed as `[Circular *N]` and prefixes that object with `<ref *N> `, numbering
//! once per top-level value. The format glue reports each class instance or recursive object
//! it starts and finishes printing (`velt_rt_strbuf_inspect_enter` / `_leave`).

use crate::strbuf::VeltStrBuf;

/// Objects being printed (`console.log`), innermost last.
struct Printing {
    /// (address, where its text starts in the builder, its `<ref *N>` number once something
    /// refers back to it, else 0).
    stack: Vec<(usize, usize, u32)>,
    /// The `<ref *N>` number of every object given one while printing the current top-level
    /// value: node numbers once per `console.log` argument, so an object keeps its number and
    /// the next cycle gets the next one, also between the elements of an array or object.
    numbered: Vec<(usize, u32)>,
}

thread_local! {
    static PRINTING: std::cell::RefCell<Printing> = const {
        std::cell::RefCell::new(Printing { stack: Vec::new(), numbered: Vec::new() })
    };
}

/// Start printing a top-level value (a `console.log` argument, a `${x}` or `String(x)`): the
/// `<ref *N>` numbering starts over at 1.
#[no_mangle]
pub extern "C" fn velt_rt_strbuf_inspect_begin() {
    PRINTING.with(|s| {
        let p = &mut *s.borrow_mut();
        // Not while an object is being printed: a value formatted from inside one (none today)
        // continues its numbering.
        if p.stack.is_empty() {
            p.numbered.clear();
        }
    })
}

/// Start printing the object at `p` (a class instance or a recursive object): 1, or, if it is
/// already being printed (the graph has a cycle), append `[Circular *N]` as node does and
/// return 0 (the caller skips the object).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_enter(buf: *mut VeltStrBuf, p: *const u8) -> u8 {
    PRINTING.with(|s| {
        let Printing { stack, numbered } = &mut *s.borrow_mut();
        let addr = p as usize;
        if stack.iter().any(|e| e.0 == addr) {
            push_circular(buf, numbered, addr);
            return 0;
        }
        stack.push((addr, (*buf).len(), 0));
        1
    })
}

/// Node's check for a container past its `depth` limit: if the object at `p` is being printed
/// (a reference back to it), append `[Circular *N]` as `velt_rt_strbuf_inspect_enter` does and
/// return 1; else 0, and the caller prints `[Name]` instead of the object. The object is not
/// entered, so it gets no `<ref *N>` prefix, as in node.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_circular(buf: *mut VeltStrBuf, p: *const u8) -> u8 {
    PRINTING.with(|s| {
        let Printing { stack, numbered } = &mut *s.borrow_mut();
        let addr = p as usize;
        if !stack.iter().any(|e| e.0 == addr) {
            return 0;
        }
        push_circular(buf, numbered, addr);
        1
    })
}

/// Append `[Circular *N]` for the object at `addr`, numbering it if it has no number yet.
unsafe fn push_circular(buf: *mut VeltStrBuf, numbered: &mut Vec<(usize, u32)>, addr: usize) {
    let n = match numbered.iter().find(|e| e.0 == addr) {
        Some(e) => e.1,
        None => {
            let n = numbered.len() as u32 + 1;
            numbered.push((addr, n));
            n
        }
    };
    (*buf).push_wtf8(format!("[Circular *{n}]").as_bytes(), None);
}

/// Done printing the innermost object started with `velt_rt_strbuf_inspect_enter`: if it has
/// a `<ref *N>` number (something refers back to it), its text gets node's `<ref *N> ` prefix.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_strbuf_inspect_leave(buf: *mut VeltStrBuf) {
    let done = PRINTING.with(|s| {
        let Printing { stack, numbered } = &mut *s.borrow_mut();
        let (addr, start, _) = stack.pop()?;
        numbered.iter().find(|e| e.0 == addr).map(|e| (start, e.1))
    });
    if let Some((start, n)) = done {
        (*buf).insert_bytes(start, format!("<ref *{n}> ").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::str::VeltStr;

    unsafe fn text(b: &VeltStrBuf) -> String {
        String::from_utf8(b.as_bytes().to_vec()).unwrap()
    }

    /// A container past node's depth limit is only checked for being a reference back.
    #[test]
    fn past_the_depth_limit_only_references_back_print() {
        unsafe {
            let mut b = VeltStr::with_capacity(0);
            let (outer, other) = (1u8, 2u8);
            velt_rt_strbuf_inspect_begin();
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &outer), 1);
            b.push_wtf8(b"A { a: ", None);
            assert_eq!(velt_rt_strbuf_inspect_circular(&mut b, &other), 0);
            b.push_wtf8(b"[B], b: ", None);
            assert_eq!(velt_rt_strbuf_inspect_circular(&mut b, &outer), 1);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(&mut b);
            assert_eq!(text(&b), "<ref *1> A { a: [B], b: [Circular *1] }");
            b.release();
        }
    }

    #[test]
    fn a_reference_back_prints_circular_and_marks_the_target() {
        unsafe {
            let mut b = VeltStr::with_capacity(0);
            let (outer, inner) = (1u8, 2u8);
            velt_rt_strbuf_inspect_begin();
            b.push_wtf8(b"x: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &outer), 1);
            b.push_wtf8(b"A { b: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 1);
            b.push_wtf8(b"B { a: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &outer), 0);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(&mut b);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(&mut b);
            assert_eq!(text(&b), "x: <ref *1> A { b: B { a: [Circular *1] } }");
            // Numbering starts over for the next value printed.
            velt_rt_strbuf_inspect_begin();
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 1);
            assert_eq!(velt_rt_strbuf_inspect_enter(&mut b, &inner), 0);
            velt_rt_strbuf_inspect_leave(&mut b);
            assert!(text(&b).ends_with("<ref *1> [Circular *1]"));
            b.release();
        }
    }

    /// Siblings inside one value (`[c, d]`, `[c, c]`) share one numbering, as in node.
    #[test]
    fn siblings_keep_one_numbering() {
        unsafe fn cyclic(b: &mut VeltStrBuf, p: &u8) {
            assert_eq!(velt_rt_strbuf_inspect_enter(b, p), 1);
            b.push_wtf8(b"C { next: ", None);
            assert_eq!(velt_rt_strbuf_inspect_enter(b, p), 0);
            b.push_wtf8(b" }", None);
            velt_rt_strbuf_inspect_leave(b);
        }
        unsafe {
            let mut b = VeltStr::with_capacity(0);
            let (c, d) = (1u8, 2u8);
            velt_rt_strbuf_inspect_begin();
            for p in [&c, &d, &c] {
                cyclic(&mut b, p);
                b.push_wtf8(b", ", None);
            }
            assert_eq!(
                text(&b),
                concat!(
                    "<ref *1> C { next: [Circular *1] }, <ref *2> C { next: [Circular *2] }, ",
                    "<ref *1> C { next: [Circular *1] }, "
                )
            );
            b.release();
        }
    }
}
