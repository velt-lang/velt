//! Arrays and tuples as `console.log` prints them, like Node: `[ 1, 2, 3 ]` (empty: `[]`),
//! `[Array]` past the depth limit, and only the first [`MAX_ARRAY_LENGTH`] elements followed by
//! `... n more items` (Node's `maxArrayLength`), so printing a huge array formats only what is
//! shown.

use velt_sema::hir::TyId;

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// Node's default `maxArrayLength` (velt_rt's `inspect::MAX_ARRAY_LENGTH`): an array, `Map` or
/// `Set` shows this many entries.
pub(super) const MAX_ARRAY_LENGTH: i128 = 100;

/// Node's `, ... n more items` after the shown entries, for a count known at compile time.
fn more_items(remaining: usize) -> String {
    let s = if remaining == 1 { "" } else { "s" };
    format!(", ... {remaining} more item{s}")
}

impl FnLower<'_, '_> {
    /// The array whose content (data pointer, length) is at `arr`, of elements of type `e`, at
    /// node's depth `depth`.
    pub(super) fn format_array(&mut self, buf: &Operand, arr: &Place, e: TyId, depth: &Operand) {
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(arr, Proj::Field(1)))),
        );
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), cint(0, Ty::U64)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        self.push_text(buf, "[]");
        self.goto(done);
        self.switch_to(full_bb);
        self.within_depth(buf, depth, "[Array]", None, |lw, child| {
            lw.format_elements(buf, arr, e, len, &child)
        });
        self.goto(done);
        self.switch_to(done);
    }

    /// `[ a, b, … ]` for a non-empty array of `len` elements: the first 100, then node's
    /// `... n more items`.
    fn format_elements(
        &mut self,
        buf: &Operand,
        arr: &Place,
        e: TyId,
        len: Operand,
        child: &Operand,
    ) {
        let limit = cint(MAX_ARRAY_LENGTH, Ty::U64);
        let long = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, len.clone(), limit.clone()),
        );
        let shown = self.temp(Ty::U64);
        self.assign(Place::local(shown), Rvalue::Use(len.clone()));
        let (cap_bb, start_bb) = (self.new_block(), self.new_block());
        self.branch(long.clone(), cap_bb, start_bb);
        self.switch_to(cap_bb);
        self.assign(Place::local(shown), Rvalue::Use(limit.clone()));
        self.goto(start_bb);
        self.switch_to(start_bb);
        self.push_text(buf, "[ ");
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, Operand::Copy(Place::local(shown)), |lw, k| {
            let first = lw.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, k.clone(), cint(0, Ty::U64)),
            );
            let (sep_bb, elem_bb) = (lw.new_block(), lw.new_block());
            lw.branch(first, elem_bb, sep_bb);
            lw.switch_to(sep_bb);
            lw.push_text(buf, ", ");
            lw.goto(elem_bb);
            lw.switch_to(elem_bb);
            let p = lw.elem_place(arr, k, e);
            lw.format_nested(buf, &p, e, child);
        });
        let (more_bb, close_bb) = (self.new_block(), self.new_block());
        self.branch(long, more_bb, close_bb);
        self.switch_to(more_bb);
        let remaining = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, len, limit));
        self.call_rt(Rt::StrbufInspectMore, vec![buf.clone(), remaining], None);
        self.goto(close_bb);
        self.switch_to(close_bb);
        self.push_text(buf, " ]");
    }

    /// A tuple, which node prints as the array it is.
    pub(super) fn format_tuple(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        tys: &[TyId],
        depth: &Operand,
    ) {
        if tys.is_empty() {
            return self.push_text(buf, "[]");
        }
        self.within_depth(buf, depth, "[Array]", None, |lw, child| {
            lw.push_text(buf, "[ ");
            for (i, &t) in tys.iter().take(MAX_ARRAY_LENGTH as usize).enumerate() {
                if i > 0 {
                    lw.push_text(buf, ", ");
                }
                let fp = lw.field_place(place, ty, i as u32);
                lw.format_nested(buf, &fp, t, &child);
            }
            if let Some(remaining) = tys.len().checked_sub(MAX_ARRAY_LENGTH as usize) {
                if remaining > 0 {
                    lw.push_text(buf, &more_items(remaining));
                }
            }
            lw.push_text(buf, " ]");
        });
    }
}
