//! Places, operands and constants: HIR local → VIR place, taking addresses (aggregates are passed
//! by pointer), temporaries, literal constants, zero values and numeric casts.

use velt_sema::hir::{self, LocalId, TyId, TyKind};

use super::{cint, ice, unit, FnLower};
use crate::vir::{Const, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

/// Wrap an integer constant to the width/signedness of `ty`.
pub(super) fn wrap_int(v: i128, ty: Ty) -> i128 {
    let bits = ty.scalar_size().unwrap_or(8) * 8;
    let modulus = 1i128 << bits;
    let m = v.rem_euclid(modulus);
    if ty.is_signed() && m >= modulus / 2 {
        m - modulus
    } else {
        m
    }
}

/// `place` extended by one projection.
pub(super) fn proj(place: &Place, p: Proj) -> Place {
    let mut q = place.clone();
    q.proj.push(p);
    q
}

impl FnLower<'_, '_> {
    pub(super) fn local_place(&mut self, id: LocalId) -> Place {
        let info = &self.info[id.0 as usize];
        let (l, indirect, ty) = (
            info.vir.unwrap_or_else(|| ice("place of a Unit local")),
            info.indirect,
            info.ty,
        );
        if indirect {
            let t = self.cx.ty(ty);
            Place {
                local: l,
                proj: vec![Proj::Deref(t)],
            }
        } else {
            Place::local(l)
        }
    }

    /// Address of a place as a `Ptr` operand.
    pub(super) fn addr(&mut self, place: Place) -> Operand {
        if let [Proj::Deref(_)] = place.proj.as_slice() {
            return Operand::Copy(Place::local(place.local));
        }
        self.rvalue_temp(Ty::Ptr, Rvalue::AddrOf(place))
    }

    /// Address of the memory holding an operand's value (constants are spilled first).
    pub(super) fn operand_addr(&mut self, op: Operand, ty: Ty) -> Operand {
        match op {
            Operand::Copy(p) => self.addr(p),
            // Only reachable in dead code (a diverging operand); any pointer will do.
            _ if ty == Ty::Unit => cint(0, Ty::Ptr),
            c @ Operand::Const(..) => {
                let t = self.temp(ty);
                self.assign(Place::local(t), Rvalue::Use(c));
                self.addr(Place::local(t))
            }
        }
    }

    /// The operand as a place (constants are spilled into a temporary).
    pub(super) fn operand_place(&mut self, op: Operand, ty: Ty) -> Place {
        match op {
            Operand::Copy(p) => p,
            c => {
                let t = self.temp(if ty == Ty::Unit { Ty::I64 } else { ty });
                if ty != Ty::Unit {
                    self.assign(Place::local(t), Rvalue::Use(c));
                }
                Place::local(t)
            }
        }
    }

    /// Snapshot a scalar local read so later side effects in the same expression can't change it.
    pub(super) fn freeze(&mut self, op: Operand, ty: TyId) -> Operand {
        let t = self.vty(ty);
        match op {
            Operand::Copy(p) if t.is_scalar() && !self.dead() => {
                self.rvalue_temp(t, Rvalue::Use(Operand::Copy(p)))
            }
            op => op,
        }
    }

    pub(super) fn rvalue_temp(&mut self, ty: Ty, rv: Rvalue) -> Operand {
        let t = self.temp(ty);
        self.assign(Place::local(t), rv);
        Operand::Copy(Place::local(t))
    }

    /// Copy a value into a fresh temporary (owned by the caller of this helper).
    pub(super) fn copy_to_temp(&mut self, v: Operand, ty: Ty) -> crate::vir::Local {
        let t = self.temp(ty);
        self.assign(Place::local(t), Rvalue::Use(v));
        t
    }

    /// Numeric conversion with Rust `as` semantics; integer constants are folded.
    pub(super) fn cast_to(&mut self, op: Operand, from: Ty, to: Ty) -> Operand {
        if from == to {
            return op;
        }
        match op {
            Operand::Const(Const::Int(v), _) if to.is_int() => cint(wrap_int(v, to), to),
            Operand::Const(Const::Int(v), _) if to.is_float() => {
                Operand::Const(Const::Float(v as f64), to)
            }
            op => self.rvalue_temp(to, Rvalue::Cast(op, to)),
        }
    }

    /// A string literal: static bytes + a `{ptr as u64, len, cap: 0}` value (never freed).
    pub(super) fn str_lit(&mut self, s: &str) -> Operand {
        let mut bytes = s.as_bytes().to_vec();
        let len = bytes.len();
        if bytes.is_empty() {
            // Never emit zero-sized data: the pointer must be valid even for "".
            bytes.push(0);
        }
        let sid = self.cx.static_bytes(bytes, 1);
        let ptr = Operand::Const(Const::Static(sid), Ty::Ptr);
        let fields = vec![
            self.rvalue_temp(Ty::U64, Rvalue::Cast(ptr, Ty::U64)),
            cint(len as i128, Ty::U64),
            cint(0, Ty::U64),
        ];
        self.rvalue_temp(Ty::Agg(STR_AGG), Rvalue::Aggregate(STR_AGG, fields))
    }

    pub(super) fn lit(&mut self, l: &hir::Lit, ty: TyId) -> Operand {
        let t = self.vty(ty);
        match l {
            hir::Lit::Int(n) if t.is_float() => Operand::Const(Const::Float(*n as f64), t),
            hir::Lit::Int(n) => cint(wrap_int(*n as i128, t), t),
            hir::Lit::Float(f) => Operand::Const(Const::Float(*f), t),
            hir::Lit::Bool(b) => Operand::Const(Const::Bool(*b), Ty::Bool),
            hir::Lit::Unit => unit(),
            hir::Lit::Str(s) => self.str_lit(s),
            hir::Lit::Null => {
                let ty = self.sub(ty);
                self.none_value(ty)
            }
        }
    }

    /// The `null` value of an option type.
    pub(super) fn none_value(&mut self, opt: TyId) -> Operand {
        match self.cx.ty(opt) {
            Ty::Ptr => cint(0, Ty::Ptr),
            Ty::Agg(a) => {
                let TyKind::Option(inner) = self.cx.kind(opt) else {
                    ice("null of a non-option type")
                };
                let vt = self.cx.ty(inner);
                let payload = self.zero_value(vt);
                let fields = vec![Operand::Const(Const::Bool(false), Ty::Bool), payload];
                self.rvalue_temp(Ty::Agg(a), Rvalue::Aggregate(a, fields))
            }
            t => ice(format_args!("null of type {t:?}")),
        }
    }

    /// An all-zero value of any VIR type (a valid "nothing to drop" state for every type).
    pub(super) fn zero_value(&mut self, t: Ty) -> Operand {
        match t {
            Ty::Agg(a) => {
                let fields: Vec<Ty> = self.cx.aggs[a.0 as usize]
                    .fields
                    .iter()
                    .map(|f| f.0)
                    .collect();
                let ops = fields.into_iter().map(|f| self.zero_value(f)).collect();
                self.rvalue_temp(t, Rvalue::Aggregate(a, ops))
            }
            Ty::F32 | Ty::F64 => Operand::Const(Const::Float(0.0), t),
            Ty::Bool => Operand::Const(Const::Bool(false), t),
            Ty::Unit => unit(),
            t => cint(0, t),
        }
    }
}
