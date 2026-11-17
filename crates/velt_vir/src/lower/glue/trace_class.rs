//! Trace glue of a class with subclasses (docs/internals/design/cycles.md section 2, step 3):
//! a value of class type `C` may be an object of any subclass, with fields `C` doesn't have, so
//! the exact glue (`Glue::Trace` of the object's class) is chosen by its dynamic class.
//!
//! An object's header word is its class's vtable, one static per concrete class. The vtable has
//! no free slot for a trace entry (its nine hidden slots are taken, and the runtime knows their
//! offsets, velt_rt `fn_props.rs`), so `Glue::TraceClass` compares the header with the vtables
//! of the classes of `C`'s hierarchy the program built that are `C` or extend it
//! (`Boxing::class_traced`), and calls the matching class's exact glue. An object of none of them
//! (a class the program never built a vtable for can't have objects) is left out, a leak at
//! worst.

use velt_sema::hir::{DefId, TyId, TyKind};

use super::{Glue, VtableKey};
use crate::lower::boxing::Boxing;
use crate::lower::{unit, Cx, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// For a class `t` in a hierarchy with a vtable, the concrete classes a value of `t` may be
    /// (`t` or a subclass) whose exact trace glue finds something (`b`: the counted types;
    /// `None`: this pass's). Empty for any other type.
    pub(in crate::lower) fn traced_classes(&mut self, b: Option<&Boxing>, t: TyId) -> Vec<TyId> {
        let TyKind::Adt(d, _) = self.kind(t) else {
            return vec![];
        };
        if !self.is_class(t) || !self.has_header(d) {
            return vec![];
        }
        let root = self.class_root(d);
        let bx = b.unwrap_or(&self.boxing);
        let built = bx.class_traced.get(&root).cloned().unwrap_or_default();
        let mut out = vec![];
        for c in built {
            let TyKind::Adt(cd, _) = self.kind(c) else {
                continue;
            };
            if (cd == d || self.extends(cd, d)) && !self.exact_refs_of(b, c).is_empty() {
                out.push(c);
            }
        }
        out
    }

    /// Does the class `sub` extend `sup`, directly or not (and is not `sup` itself)?
    pub(in crate::lower) fn extends(&self, mut sub: DefId, sup: DefId) -> bool {
        while let Some(b) = self.adt_def(sub).base {
            let TyKind::Adt(bd, _) = self.kind(b) else {
                return false;
            };
            if bd == sup {
                return true;
            }
            sub = bd;
        }
        false
    }
}

impl FnLower<'_, '_> {
    /// Body of `Glue::TraceClass` for the class `ty` (module docs).
    pub(super) fn trace_class_body(
        &mut self,
        obj: vir::Local,
        visit: vir::Local,
        ctx: vir::Local,
        ty: TyId,
    ) {
        let classes = self.cx.traced_classes(None, ty);
        let o = Operand::Copy(Place::local(obj));
        let vt = self.obj_vtable(o.clone(), ty);
        let done = self.new_block();
        for c in classes {
            let want = self.vtable_addr(VtableKey::Class(c));
            let is = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, vt.clone(), want));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(is, yes, no);
            self.switch_to(yes);
            let args = vec![
                o.clone(),
                Operand::Copy(Place::local(visit)),
                Operand::Copy(Place::local(ctx)),
            ];
            self.call_glue(Glue::Trace, c, args);
            self.goto(done);
            self.switch_to(no);
        }
        self.goto(done);
        self.switch_to(done);
        self.terminate(Terminator::Return(unit()));
    }
}
