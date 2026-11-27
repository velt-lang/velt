//! String views (#3469 step 1): the views mode of the family analysis.
//!
//! `let rest = text; … rest = rest.slice(n)` clones `text` into `rest` and slices `rest` again
//! and again: a runtime call, a count increment on the shared buffer and a release of the old
//! slice per step. When `text` stays alive and unchanged while the family's text is read (the
//! analysis of the module doc, with slices joining the family), the clone that starts the
//! family is replaced by a *demotion*: if `text` is a heap ASCII string, `rest` gets the static
//! form of the same bytes, `{w0, w1, 0}` (rt_abi.md "Strings"), with no count; otherwise the
//! clone runs as before. A static string slices to static strings, inline in std's `sliceOf`
//! (and in the runtime), and drops as nothing, so the loop has no call and no count left.
//!
//! What the mode changes in the analysis (`Cx::views`):
//! - **Members may hold views, owned strings and anything else**: their drops stay (a drop of a
//!   static string does nothing), and `check_member_defs` only asks for a demotion.
//! - **What may hold a view joins the family**: the output of `velt_rt_str_slice` of a member,
//!   and the copy a clone of a member makes (a bitwise copy of a view is a view). A read-only
//!   call cannot return a string (VIR has no aggregate results, and a call that only reads has no
//!   out-pointer to write one through), but its 64-bit integer result could be a data word, which
//!   is then tracked; an out-pointer that is not a string local's address lets a view out, and
//!   the family is left alone.
//! - **Every other way out bails**, as in the copy mode: a store to memory (a field, an array
//!   element, a closure environment, a frame across `await` or `yield`), a return, a call that
//!   is not read-only, a memory copy. Those are the escapes; a later step promotes the view
//!   there instead of bailing.
//! - **A slice keeps the source**: `clobbers` lets a slice into a member run while the family is
//!   live, as it lets clones and drops of strings in locals run; it allocates or shares, and
//!   never frees or writes anything but its output.
//!
//! Only ASCII is demoted: a non-ASCII static string is taken for a literal by the position
//! translation (`velt_rt::str::recent`, `std/lang/string/positions.velt`), which remembers it
//! for good. The demotion tests the form at run time, so a non-ASCII or inline `text` keeps the
//! clone and the family holds what it holds today.

use velt_vir::vir::{
    BasicBlock, BinOp, BlockId, Callee, Const, Function, Local, LocalDecl, Operand, Place, Proj,
    Rvalue, Stmt, Terminator, Ty, STR_AGG,
};

use super::{Cx, Family};
use crate::srclocs::push_stmt;

impl Cx<'_> {
    /// Views mode: the result `d` of a read-only call (`callee`) that took a family value. No
    /// call returns a string itself (VIR keeps aggregates out of signatures: a string result is
    /// written through an out-pointer, which a call that only reads does not have). A Velt
    /// function's 64-bit integer result could be a data word (std computes one with
    /// `strWord0`), so a register holding one is tracked as a family pointer; a runtime function
    /// returns no address of its arguments as an integer (its results are counts, positions,
    /// code units, hashes), so tracking those would only taint every character a loop reads. A
    /// narrower scalar carries no view; anything else (a pointer, an aggregate, a place in memory)
    /// could carry one out, so the family is left alone.
    pub(super) fn call_result(&self, f: &mut Family, callee: &Callee, d: &Place) {
        let ty = crate::locals::place_ty(self.aggs, self.func, d);
        let wide = matches!(ty, Some(Ty::U64 | Ty::I64));
        if wide && self.rt.returns_no_address(callee) {
            return;
        }
        let register = d.proj.is_empty() && self.usage.is_register(d.local);
        if wide && register {
            f.pointers.insert(d.local);
            return;
        }
        f.bail |= !matches!(
            ty,
            Some(
                Ty::Bool
                    | Ty::U8
                    | Ty::I8
                    | Ty::U16
                    | Ty::I16
                    | Ty::U32
                    | Ty::I32
                    | Ty::F32
                    | Ty::F64
                    | Ty::Unit
            )
        );
    }
}

/// Replace the clone `velt_rt_str_clone(src, &m)` ending block `bi` of `func` with a demotion:
/// `*src`'s words with a zero form word when `*src` is a heap ASCII string (`w2 > 0` as `i64`:
/// a plain heap string or a slice; units equal bytes), the clone otherwise.
pub(super) fn demote(func: &mut Function, bi: usize, m: Local, src: Local) {
    let Terminator::Call { next, .. } = &func.blocks[bi].term else {
        panic!("ICE: string views: a demotion's clone is gone");
    };
    let next = *next;
    let at = func.term_loc(bi);
    let local = |func: &mut Function, ty| {
        func.locals.push(LocalDecl::new(ty, None));
        Local(func.locals.len() as u32 - 1)
    };
    let (w1, w2, signed, heap, units, bytes, ascii, ok) = (
        local(func, Ty::U64),
        local(func, Ty::U64),
        local(func, Ty::I64),
        local(func, Ty::Bool),
        local(func, Ty::U64),
        local(func, Ty::U64),
        local(func, Ty::Bool),
        local(func, Ty::Bool),
    );
    let word = |k| {
        Operand::Copy(Place {
            local: src,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG)), Proj::Field(k)],
        })
    };
    let reg = |l: Local| Operand::Copy(Place::local(l));
    let int = |v: u64| Operand::Const(Const::Int(i128::from(v)), Ty::U64);
    let set = |l: Local, rv| Stmt::Assign(Place::local(l), rv);
    // The view, in a block of its own: `m = {w0, w1, 0}`.
    let view = BlockId(func.blocks.len() as u32);
    let field = |k| Place {
        local: m,
        proj: vec![Proj::Field(k)],
    };
    func.blocks.push(BasicBlock {
        stmts: vec![
            Stmt::Assign(field(0), Rvalue::Use(word(0))),
            Stmt::Assign(field(1), Rvalue::Use(reg(w1))),
            Stmt::Assign(field(2), Rvalue::Use(int(0))),
        ],
        term: Terminator::Goto(next),
    });
    if !func.locs.is_empty() {
        func.locs.push(vec![at; 4]);
    }
    // The clone, as it was, in a block of its own.
    let clone = BlockId(func.blocks.len() as u32);
    let test = Terminator::Branch {
        cond: reg(ok),
        then: view,
        els: clone,
    };
    let term = std::mem::replace(&mut func.blocks[bi].term, test);
    func.blocks.push(BasicBlock {
        stmts: Vec::new(),
        term,
    });
    if !func.locs.is_empty() {
        func.locs.push(vec![at]);
    }
    let tests = [
        set(w1, Rvalue::Use(word(1))),
        set(w2, Rvalue::Use(word(2))),
        set(signed, Rvalue::Cast(reg(w2), Ty::I64)),
        set(
            heap,
            Rvalue::Binary(
                BinOp::Gt,
                reg(signed),
                Operand::Const(Const::Int(0), Ty::I64),
            ),
        ),
        set(units, Rvalue::Binary(BinOp::UShr, reg(w1), int(32))),
        set(
            bytes,
            Rvalue::Binary(BinOp::BitAnd, reg(w1), int(0xffff_ffff)),
        ),
        set(ascii, Rvalue::Binary(BinOp::Eq, reg(units), reg(bytes))),
        set(ok, Rvalue::Binary(BinOp::BitAnd, reg(heap), reg(ascii))),
    ];
    for s in tests {
        push_stmt(func, bi, s, at);
    }
}
