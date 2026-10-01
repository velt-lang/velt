//! Patterns: `test_pat` emits the checks (continuing in the current block on a match, jumping to
//! `fail` otherwise), `bind_pat` binds the pattern's locals (by-value copies/moves, or pointers
//! for by-reference bindings), and `drop_rest` drops whatever a pattern did not move out of an
//! owned value.

use velt_sema::hir::{self, Lit, Pat, PatKind, TyId, TyKind, UseMode};

use super::operand::proj;
use super::{cint, ice, FnLower};
use crate::vir::{BinOp, BlockId, Operand, Place, Proj, Rvalue, Ty};

/// Does the pattern move anything out of the matched value?
pub(super) fn has_moves(p: &Pat) -> bool {
    match &p.kind {
        PatKind::Binding(_, m) => *m == UseMode::Move,
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().any(has_moves)
        }
        PatKind::Adt { fields } => fields.iter().any(|(_, q)| has_moves(q)),
        PatKind::Array { elems, .. } => elems.iter().any(has_moves),
        PatKind::Some(q) => has_moves(q),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => false,
    }
}

impl FnLower<'_, '_> {
    /// Place + concrete type of payload `k` of variant `v` of the enum/result value at `place`.
    pub(super) fn variant_part(
        &mut self,
        place: &Place,
        ty: TyId,
        v: u32,
        k: usize,
    ) -> (Place, TyId) {
        let tys = self.cx.variant_tys(ty, v);
        let view = self.cx.view(ty, v);
        // Unit payloads have no field.
        let mut field = 1;
        for t in &tys[..k] {
            if !self.cx.is_unit(*t) {
                field += 1;
            }
        }
        let p = proj(&proj(place, Proj::Cast(view)), Proj::Field(field));
        (p, tys[k])
    }

    fn cond_jump(&mut self, ok: Operand, fail: BlockId) {
        let next = self.new_block();
        self.branch(ok, next, fail);
        self.switch_to(next);
    }

    fn cmp(&mut self, op: BinOp, a: Operand, b: Operand) -> Operand {
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(op, a, b))
    }

    pub(super) fn test_pat(&mut self, pat: &Pat, place: &Place, ty: TyId, fail: BlockId) {
        if self.dead() {
            return;
        }
        match &pat.kind {
            PatKind::Wildcard | PatKind::Binding(..) => {}
            PatKind::Lit(l) => {
                let ok = self.lit_eq(l, place, ty);
                self.cond_jump(ok, fail);
            }
            PatKind::Variant { variant, args, .. } => {
                self.test_variant(*variant, args, place, ty, fail)
            }
            PatKind::Adt { fields } => {
                for (i, p) in fields {
                    let fty = self.cx.adt_field_tys(ty)[*i as usize];
                    let fp = self.field_place(place, ty, *i);
                    self.test_pat(p, &fp, fty, fail);
                }
            }
            PatKind::Tuple(ps) => {
                let TyKind::Tuple(tys) = self.cx.kind(ty) else {
                    ice("tuple pattern")
                };
                for (i, p) in ps.iter().enumerate() {
                    let fp = self.field_place(place, ty, i as u32);
                    self.test_pat(p, &fp, tys[i], fail);
                }
            }
            PatKind::Array { elems, rest } => {
                let len = Operand::Copy(proj(place, Proj::Field(1)));
                let op = if rest.is_some() { BinOp::Ge } else { BinOp::Eq };
                let ok = self.cmp(op, len, cint(elems.len() as i128, Ty::U64));
                self.cond_jump(ok, fail);
                let elem = self.elem_ty(ty);
                for (i, p) in elems.iter().enumerate() {
                    let ep = self.elem_place(place, cint(i as i128, Ty::U64), elem);
                    self.test_pat(p, &ep, elem, fail);
                }
            }
            PatKind::Or(alts) => self.test_or(alts, place, ty, fail),
            PatKind::None => {
                let some = self.option_is_some(place, ty);
                let next = self.new_block();
                self.branch(some, fail, next);
                self.switch_to(next);
            }
            PatKind::Some(p) => {
                let some = self.option_is_some(place, ty);
                self.cond_jump(some, fail);
                let TyKind::Option(inner) = self.cx.kind(ty) else {
                    ice("Some pattern")
                };
                let payload = self.some_payload(place, ty);
                self.test_pat(p, &payload, inner, fail);
            }
        }
    }

    fn test_or(&mut self, alts: &[Pat], place: &Place, ty: TyId, fail: BlockId) {
        let ok = self.new_block();
        for (i, alt) in alts.iter().enumerate() {
            let next = if i + 1 == alts.len() {
                fail
            } else {
                self.new_block()
            };
            self.test_pat(alt, place, ty, next);
            self.goto(ok);
            if next != fail {
                self.switch_to(next);
            }
        }
        self.switch_to(ok);
    }

    fn test_variant(&mut self, v: u32, args: &[Pat], place: &Place, ty: TyId, fail: BlockId) {
        let tag = match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_c_like_enum(d) => {
                let disc = self.cx.enum_def(d).variants[v as usize].discriminant;
                let ok = self.cmp(
                    BinOp::Eq,
                    Operand::Copy(place.clone()),
                    cint(disc as i128, Ty::I64),
                );
                self.cond_jump(ok, fail);
                return;
            }
            TyKind::Adt(..) => Ty::U32,
            k => ice(format_args!("variant pattern on {k:?}")),
        };
        let t = Operand::Copy(proj(place, Proj::Field(0)));
        let ok = self.cmp(BinOp::Eq, t, cint(v as i128, tag));
        self.cond_jump(ok, fail);
        for (k, p) in args.iter().enumerate() {
            let (sp, st) = self.variant_part(place, ty, v, k);
            self.test_pat(p, &sp, st, fail);
        }
    }

    /// `value == literal` for a literal pattern (strings: `velt_rt_str_eq`).
    fn lit_eq(&mut self, l: &Lit, place: &Place, ty: TyId) -> Operand {
        if let Lit::Null = l {
            let some = self.option_is_some(place, ty);
            return self.rvalue_temp(Ty::Bool, Rvalue::Unary(crate::vir::UnOp::Not, some));
        }
        let v = Operand::Copy(place.clone());
        let c = self.lit(l, ty);
        if let TyKind::Str = self.cx.kind(ty) {
            let (a, b) = (
                self.addr(place.clone()),
                self.operand_addr(c, Ty::Agg(crate::vir::STR_AGG)),
            );
            return self.str_eq(a, b);
        }
        self.cmp(BinOp::Eq, v, c)
    }

    /// Bind the pattern's locals to the parts of the (already matched) value at `place`.
    /// Moved-out bindings become owned by the current scope.
    pub(super) fn bind_pat(&mut self, pat: &Pat, place: &Place, ty: TyId, register: bool) {
        if self.dead() {
            return;
        }
        // Parts of a counted value have other owners: moving bindings inside it take shares of
        // them (a binding of the whole value just takes it over).
        let outer = self.share_binds;
        self.share_binds |= self.cx.counted(ty) && !matches!(pat.kind, PatKind::Binding(..));
        self.bind_parts(pat, place, ty, register);
        self.share_binds = outer;
    }

    fn bind_parts(&mut self, pat: &Pat, place: &Place, ty: TyId, register: bool) {
        match &pat.kind {
            PatKind::Binding(id, mode) => {
                let moving = *mode == UseMode::Move;
                self.bind_local(*id, moving && register, moving, place)
            }
            PatKind::Variant { variant, args, .. } => {
                for (k, p) in args.iter().enumerate() {
                    let (sp, st) = self.variant_part(place, ty, *variant, k);
                    self.bind_pat(p, &sp, st, register);
                }
            }
            PatKind::Adt { fields } => {
                for (i, p) in fields {
                    let fty = self.cx.adt_field_tys(ty)[*i as usize];
                    let fp = self.field_place(place, ty, *i);
                    self.bind_pat(p, &fp, fty, register);
                }
            }
            PatKind::Tuple(ps) => {
                let TyKind::Tuple(tys) = self.cx.kind(ty) else {
                    ice("tuple pattern")
                };
                for (i, p) in ps.iter().enumerate() {
                    let fp = self.field_place(place, ty, i as u32);
                    self.bind_pat(p, &fp, tys[i], register);
                }
            }
            PatKind::Array { elems, rest } => {
                let elem = self.elem_ty(ty);
                let place = &self.content(place, ty);
                // A destructuring `let` is not tested first: an array shorter than the pattern
                // panics like indexing its first missing element (`xs[2]`).
                if let Some(last) = elems.len().checked_sub(1) {
                    self.elem_place_checked(place, ty, cint(last as i128, Ty::U64), Ty::U64);
                }
                for (i, p) in elems.iter().enumerate() {
                    let ep = self.elem_place(place, cint(i as i128, Ty::U64), elem);
                    self.bind_pat(p, &ep, elem, register);
                }
                if let Some(r) = rest {
                    self.bind_rest(*r, place, ty, elems.len());
                }
            }
            PatKind::Some(p) => {
                let TyKind::Option(inner) = self.cx.kind(ty) else {
                    ice("Some pattern")
                };
                let payload = self.some_payload(place, ty);
                self.bind_pat(p, &payload, inner, register);
            }
            PatKind::Or(alts) => {
                if alts.iter().any(binds_anything) {
                    ice("bindings inside or-patterns are not supported");
                }
            }
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => {}
        }
    }

    /// `owns`: the binding becomes owned by the current scope now; `moving`: it takes the part
    /// out of the matched value (a share inside a counted value, `bind_pat`).
    fn bind_local(&mut self, id: hir::LocalId, owns: bool, moving: bool, place: &Place) {
        let info = &self.info[id.0 as usize];
        let Some(l) = info.vir else { return };
        if info.indirect {
            let a = self.addr(place.clone());
            self.assign(Place::local(l), Rvalue::Use(a));
            return;
        }
        let v = match moving && self.share_binds {
            true => {
                let ty = self.info[id.0 as usize].ty;
                self.share_value(Operand::Copy(place.clone()), ty)
            }
            false => Operand::Copy(place.clone()),
        };
        self.assign(Place::local(l), Rvalue::Use(v));
        if owns && self.info[id.0 as usize].droppable {
            self.mark_init(id);
            self.register_local_drop(id);
        }
    }

    /// `[a, b, ...rest]`: `rest` gets a new array of the remaining elements (shared, like JS).
    fn bind_rest(&mut self, r: hir::LocalId, place: &Place, ty: TyId, skip: usize) {
        let Some(l) = self.info[r.0 as usize].vir else {
            return;
        };
        let elem = self.elem_ty(ty);
        let len = Operand::Copy(proj(place, Proj::Field(1)));
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, len.clone(), cint(skip as i128, Ty::U64)),
        );
        let fresh = self.inline_array_with_len(n.clone(), elem);
        let fp = Place::local(fresh);
        self.copy_elems(place, cint(skip as i128, Ty::U64), &fp, n, elem, true);
        let v = self.box_value(Operand::Copy(fp), ty);
        self.assign(Place::local(l), Rvalue::Use(v));
        if self.info[r.0 as usize].droppable {
            self.mark_init(r);
            self.register_local_drop(r);
        }
    }

    /// Drop the parts of the owned value at `place` that `pat` did not move out.
    pub(super) fn drop_rest(&mut self, place: Place, ty: TyId, pat: &Pat) {
        // Bindings out of a counted value are shares (`bind_pat`): it is released whole.
        let inside = self.cx.counted(ty) && !matches!(pat.kind, PatKind::Binding(..));
        if !has_moves(pat) || inside {
            self.drop_glue(place, ty);
            return;
        }
        match &pat.kind {
            PatKind::Binding(..) => {}
            PatKind::Variant { variant, args, .. } => {
                for (k, p) in args.iter().enumerate() {
                    let (sp, st) = self.variant_part(&place, ty, *variant, k);
                    self.drop_rest(sp, st, p);
                }
            }
            PatKind::Adt { fields } => self.drop_rest_fields(place, ty, fields),
            PatKind::Tuple(ps) => {
                let TyKind::Tuple(tys) = self.cx.kind(ty) else {
                    ice("tuple pattern")
                };
                for (i, p) in ps.iter().enumerate() {
                    let fp = self.field_place(&place, ty, i as u32);
                    self.drop_rest(fp, tys[i], p);
                }
            }
            PatKind::Some(p) => {
                let TyKind::Option(inner) = self.cx.kind(ty) else {
                    ice("Some pattern")
                };
                let payload = self.some_payload(&place, ty);
                self.drop_rest(payload, inner, p);
            }
            PatKind::Array { elems, .. } => self.drop_rest_array(place, ty, elems),
            _ => ice("moves inside or-patterns are not supported"),
        }
    }

    fn drop_rest_fields(&mut self, place: Place, ty: TyId, fields: &[(u32, Pat)]) {
        let tys = self.cx.adt_field_tys(ty);
        for (i, fty) in tys.into_iter().enumerate() {
            let fp = self.field_place(&place, ty, i as u32);
            match fields.iter().find(|(j, _)| *j as usize == i) {
                Some((_, p)) => self.drop_rest(fp, fty, p),
                None => self.drop_glue(fp, fty),
            }
        }
        if self.cx.is_class(ty) {
            self.object_free(Operand::Copy(place), ty);
        }
    }

    fn drop_rest_array(&mut self, place: Place, ty: TyId, elems: &[Pat]) {
        let elem = self.elem_ty(ty);
        for (i, p) in elems.iter().enumerate() {
            let ep = self.elem_place(&place, cint(i as i128, Ty::U64), elem);
            self.drop_rest(ep, elem, p);
        }
        if self.cx.needs_drop(elem) {
            let k = self.temp(Ty::U64);
            self.assign(
                Place::local(k),
                Rvalue::Use(cint(elems.len() as i128, Ty::U64)),
            );
            let len = Operand::Copy(proj(&place, Proj::Field(1)));
            self.count_loop(k, len, |lw, k| {
                let ep = lw.elem_place(&place, k, elem);
                lw.drop_glue(ep, elem);
            });
        }
        self.free_buffer(&place, elem);
    }
}

fn binds_anything(p: &Pat) -> bool {
    match &p.kind {
        PatKind::Binding(..) => true,
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().any(binds_anything)
        }
        PatKind::Adt { fields } => fields.iter().any(|(_, q)| binds_anything(q)),
        PatKind::Array { elems, rest } => rest.is_some() || elems.iter().any(binds_anything),
        PatKind::Some(q) => binds_anything(q),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => false,
    }
}
