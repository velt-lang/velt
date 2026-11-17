//! Trace glue through interface values (docs/internals/design/cycles.md section 2, step 3): an
//! interface value `{ data, table }` may hold a value of any type an implementation table of its
//! interface was built for, and its static type doesn't say which.
//!
//! The table is one static per (implementation, concrete type) (`VtableKey::Impl`), so the glue
//! compares it with the tables the program built for the interface (`Boxing::iface_traced`, from
//! the table facts in a program with weak references) and handles the data by what the matching
//! type keeps there (glue/drop.rs `dyn_drop_body`):
//! - a counted object (a counted class, a boxed value): a child, visited with its trace glue;
//! - an uncounted class object or a box of its own, which only this interface value refers to:
//!   part of the owner, walked in place (`Glue::Trace` of the class, `Glue::TraceIn` of the
//!   boxed value);
//! - a primitive's shared box: nothing to visit.
//!
//! A table that matches none (an interface value of a kind this program never built) is left
//! out, a leak at worst. Interfaces whose tables keep `any` tables when `object` is weakly held
//! are traced through those (weak_object.rs) and dynamic values are not looked into here.

use velt_sema::hir::{DefId, TyId, TyKind};

use super::{Glue, VtableKey};
use crate::lower::boxing::Boxing;
use crate::lower::operand::proj;
use crate::lower::{Cx, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

/// What an interface value's data is for a concrete type (module docs).
enum Data {
    Counted,
    Owned,
    Nothing,
}

impl Cx<'_> {
    /// The tables of interface value type `t` whose data the trace glue looks into, as
    /// `(impl index, concrete type)`; empty unless `t` is an interface value traced this way.
    pub(in crate::lower) fn traced_tables(
        &mut self,
        b: Option<&Boxing>,
        t: TyId,
    ) -> Vec<(u32, TyId)> {
        let TyKind::Dyn(d, _) = self.kind(t) else {
            return vec![];
        };
        if self.is_dynamic(t) || self.is_generic_fn_value(t) {
            return vec![];
        }
        let bx = b.unwrap_or(&self.boxing);
        if bx.weak_object && bx.dyn_to_any.contains(&d) {
            return vec![];
        }
        self.iface_tables_of(bx, d)
    }

    fn iface_tables_of(&self, bx: &Boxing, d: DefId) -> Vec<(u32, TyId)> {
        bx.iface_traced.get(&d).cloned().unwrap_or_default()
    }

    /// What an interface value's data is for the concrete type `c` (module docs).
    fn dyn_data(&mut self, b: Option<&Boxing>, c: TyId) -> Data {
        let counted = match b {
            Some(b) => self.counted_in(b, c),
            None => self.counted(c),
        };
        if counted {
            Data::Counted
        } else if self.shared_dyn_box(c) {
            Data::Nothing
        } else {
            Data::Owned
        }
    }

    /// The counted types an interface value holding a `c` refers to directly: `c` itself when
    /// it is counted, else what its data, which the interface value owns, refers to.
    pub(in crate::lower) fn iface_refs(&mut self, b: Option<&Boxing>, c: TyId) -> Vec<TyId> {
        match self.dyn_data(b, c) {
            Data::Counted => vec![c],
            Data::Nothing => vec![],
            Data::Owned if self.is_class(c) => self.exact_refs_of(b, c),
            Data::Owned => self.weak_refs_inline(b, c),
        }
    }

    /// The interfaces the program built tables for, by interface, from this pass's facts
    /// (sorted, so the fixed point compares equal across passes).
    pub(in crate::lower) fn tables_by_iface(
        &self,
    ) -> rustc_hash::FxHashMap<DefId, Vec<(u32, TyId)>> {
        let mut all: Vec<(DefId, u32, TyId)> = self.facts.iface_tables.iter().copied().collect();
        all.sort_by_key(|&(d, i, t)| (d.0, i, t.0));
        let mut out: rustc_hash::FxHashMap<DefId, Vec<(u32, TyId)>> = Default::default();
        for (d, i, t) in all {
            out.entry(d).or_default().push((i, t));
        }
        out
    }
}

impl FnLower<'_, '_> {
    /// The interface value of type `t` at `place`: its data, by the table it has (module docs).
    pub(super) fn trace_iface_value(
        &mut self,
        place: &Place,
        t: TyId,
        tv: (vir::Local, vir::Local),
    ) {
        let tables: Vec<(u32, TyId)> = (self.cx.traced_tables(None, t).into_iter())
            .filter(|&(_, c)| !self.cx.iface_refs(None, c).is_empty())
            .collect();
        if tables.is_empty() {
            return;
        }
        let table = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
        );
        let data = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(0)))),
        );
        let done = self.new_block();
        for (index, c) in tables {
            let want = self.vtable_addr(VtableKey::Impl(index, c));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, table.clone(), want));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            let (visit, ctx) = (
                Operand::Copy(Place::local(tv.0)),
                Operand::Copy(Place::local(tv.1)),
            );
            match self.cx.dyn_data(None, c) {
                Data::Counted => {
                    let glue = self.trace_fn(c);
                    self.call_entry(
                        visit,
                        vec![ctx, data.clone(), glue],
                        vec![Ty::Ptr; 3],
                        Ty::Unit,
                    );
                }
                Data::Owned if self.cx.is_class(c) => {
                    self.call_glue(Glue::Trace, c, vec![data.clone(), visit, ctx]);
                }
                Data::Owned => {
                    self.call_glue(Glue::TraceIn, c, vec![data.clone(), visit, ctx]);
                }
                Data::Nothing => {}
            }
            self.goto(done);
            self.switch_to(no);
        }
        self.goto(done);
        self.switch_to(done);
    }
}
