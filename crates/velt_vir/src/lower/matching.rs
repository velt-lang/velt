//! `match` expressions (arms tested in order: pattern checks, bindings, guard — or entered through
//! one jump table when they only select by tag / integer, match_switch.rs) and destructuring
//! `let`. An owned scrutinee is dropped per arm, minus the parts the arm's pattern moved into
//! bindings (pattern.rs `drop_rest`).

use std::rc::Rc;

use velt_sema::hir::{self, Pat, TyId, UseMode};

use super::{FnLower, ScopeKind};
use crate::vir::{BlockId, Local, Operand, Place, Terminator, Ty};

/// What every arm of one `match` shares.
struct ArmCx<'p> {
    /// The scrutinee.
    place: &'p Place,
    ty: TyId,
    /// The scrutinee is owned by the match (dropped per arm, minus what the arm moved out).
    owned: bool,
    res: Option<Local>,
    join: BlockId,
}

impl FnLower<'_, '_> {
    pub(super) fn match_expr(
        &mut self,
        scrutinee: &hir::Expr,
        arms: &[hir::Arm],
        ty: TyId,
    ) -> Operand {
        let sty = self.sub(scrutinee.ty);
        self.push_scope(ScopeKind::Temps);
        let v = self.expr(scrutinee);
        let svt = self.cx.ty(sty);
        let sp = self.place_of(v, sty);
        let moved_local = matches!(&scrutinee.kind,
            hir::ExprKind::Local(id, UseMode::Move) if self.info[id.0 as usize].droppable);
        let owned = moved_local || self.take_temp(&sp);
        // A moved-from local may be reassigned inside an arm: match on a private copy.
        let sp = if moved_local && svt != Ty::Unit {
            Place::local(self.copy_to_temp(Operand::Copy(sp), svt))
        } else {
            sp
        };
        let rt = self.vty(ty);
        let res = (rt != Ty::Unit).then(|| self.temp(rt));
        let arm = ArmCx {
            place: &sp,
            ty: sty,
            owned,
            res,
            join: self.new_block(),
        };
        let pats: Vec<&Pat> = arms.iter().map(|a| &a.pat).collect();
        let plan = arms
            .iter()
            .all(|a| a.guard.is_none())
            .then(|| self.switch_plan(&sp, sty, &pats))
            .flatten();
        match plan {
            Some(plan) => {
                let blocks: Vec<_> = arms.iter().map(|_| self.new_block()).collect();
                let unreachable = self.new_block();
                self.emit_switch(plan, &blocks, unreachable);
                for (a, b) in arms.iter().zip(blocks) {
                    self.switch_to(b);
                    self.push_scope(ScopeKind::Block);
                    self.bind_pat(&a.pat, &sp, sty, false);
                    self.arm_body(a, &arm);
                }
                self.switch_to(unreachable);
            }
            None => {
                for a in arms {
                    let next = self.new_block();
                    self.push_scope(ScopeKind::Block);
                    self.test_pat(&a.pat, &sp, sty, next);
                    self.bind_pat(&a.pat, &sp, sty, false);
                    if let Some(g) = &a.guard {
                        self.push_scope(ScopeKind::Temps);
                        let c = self.expr(g);
                        self.pop_scope();
                        let ok = self.new_block();
                        self.branch(c, ok, next);
                        self.switch_to(ok);
                    }
                    self.arm_body(a, &arm);
                    self.switch_to(next);
                }
            }
        }
        // Sema guarantees exhaustiveness.
        self.terminate(Terminator::Unreachable);
        self.switch_to(arm.join);
        self.pop_scope();
        let ty = self.sub(ty);
        self.owned_result(res, ty)
    }

    /// The rest of a matched arm (its bindings are bound, in the arm's scope): take ownership
    /// of what it moved out, evaluate the body into the result, close the scope, go to `join`.
    fn arm_body(&mut self, a: &hir::Arm, cx: &ArmCx) {
        if cx.owned {
            self.own_bindings(&a.pat);
            self.own_rest(cx.place.clone(), cx.ty, Rc::new(a.pat.clone()));
        }
        self.push_scope(ScopeKind::Temps);
        let v = self.consume(&a.body);
        if let Some(r) = cx.res {
            self.store(Place::local(r), v);
        }
        self.pop_scope();
        self.pop_scope();
        self.goto(cx.join);
    }

    /// Moved-out bindings of a matched arm become owned by the current scope.
    fn own_bindings(&mut self, pat: &Pat) {
        use hir::PatKind as P;
        match &pat.kind {
            P::Binding(id, UseMode::Move) => {
                if self.info[id.0 as usize].droppable && !self.dead() {
                    self.mark_init(*id);
                    self.register_local_drop(*id);
                }
            }
            P::Variant { args: ps, .. } | P::Tuple(ps) => {
                ps.iter().for_each(|p| self.own_bindings(p))
            }
            P::Adt { fields } => fields.iter().for_each(|(_, p)| self.own_bindings(p)),
            P::Array { elems, .. } => elems.iter().for_each(|p| self.own_bindings(p)),
            P::Some(p) => self.own_bindings(p),
            _ => {}
        }
    }

    /// `let <pattern> = init;` (irrefutable): the value is owned by a hidden local.
    pub(super) fn let_pat(&mut self, pat: &Pat, init: &hir::Expr) {
        let ty = self.sub(init.ty);
        if matches!(pat.kind, hir::PatKind::Binding(_, UseMode::Borrow)) && local_place(init) {
            // `const x = node.left` bound by reference (sema `const_borrow`): no copy, and the
            // place keeps ownership.
            let v = self.expr(init);
            if !self.dead() {
                let p = self.place_of(v, ty);
                self.bind_pat(pat, &p, ty, false);
            }
            return;
        }
        self.push_scope(ScopeKind::Temps);
        let v = self.consume(init);
        self.pop_scope();
        if self.dead() {
            return;
        }
        let vt = self.cx.ty(ty);
        if vt == Ty::Unit {
            return;
        }
        let s = self.copy_to_temp(v, vt);
        let sp = Place::local(s);
        self.bind_pat(pat, &sp, ty, true);
        self.own_rest(sp, ty, Rc::new(pat.clone()));
    }
}

/// Is `e` a place rooted at a local (fields, elements and payloads of it)?
fn local_place(e: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match &e.kind {
        K::Local(..) => true,
        K::Field { base, .. }
        | K::Index { base, .. }
        | K::UnwrapSome(base, _)
        | K::UnwrapVariant { expr: base, .. } => local_place(base),
        _ => false,
    }
}
