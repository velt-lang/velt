//! Tables: module constants that are array literals of numbers or booleans, read where they
//! stand from read-only data emitted once (split from `expr.rs`).

use velt_sema::hir::{self, FloatTy};

use super::{cint, FnLower};
use crate::vir::{self, Const, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `e` when it is a table (a module constant that is an array literal of number or boolean
    /// constants), for a use that only reads it where it stands: an element, the length, an
    /// iteration. Its elements are read-only data emitted once, and the result is the place of
    /// the inline header `{ data, len, cap: 0 }` over them (`cap == 0`: the buffer is not owned, so nothing
    /// frees it), also where arrays of this type are counted boxes: the header never leaves the
    /// code of those three reads, so nothing counts it.
    ///
    /// Only those three uses get it. Anywhere else (an argument, a receiver, a value that is
    /// kept) the constant is evaluated like any other, to a new array. Sema keeps a table that a
    /// use could write to in the module state (a slot, never read from here) or rejects the use,
    /// but read-only data must never be reachable from code that writes, so this does not rely
    /// on it.
    ///
    /// `None`, for the initializer to be evaluated, also when the array is empty (nothing to
    /// store) or an element is not one `table_const` folds.
    pub(super) fn table_read(&mut self, e: &hir::Expr) -> Option<Place> {
        let (data, len) = self.table_data(e)?;
        let a = self.cx.array_agg();
        let t = self.temp(Ty::Agg(a));
        self.assign(
            Place::local(t),
            Rvalue::Aggregate(
                a,
                vec![
                    Operand::Const(Const::Static(data), Ty::Ptr),
                    cint(len as i128, Ty::U64),
                    cint(0, Ty::U64),
                ],
            ),
        );
        Some(Place::local(t))
    }

    /// Whether `e` names a table that lowering stores as read-only data (`table_read`).
    pub(super) fn is_table(&mut self, e: &hir::Expr) -> bool {
        self.table_data(e).is_some()
    }

    /// The read-only data of the table `e` names, and its length (stored on first use).
    fn table_data(&mut self, e: &hir::Expr) -> Option<(vir::StaticId, usize)> {
        let hir::ExprKind::Global(d) = e.kind else {
            return None;
        };
        let (data, len) = match self.cx.tables.get(&d) {
            Some(known) => (*known)?,
            None => {
                let hir::Def::Global(g) = self.cx.hir.def(d) else {
                    return None;
                };
                // A module-level `let` (its slot holds the array, which code may replace or
                // modify) is never read from the stored initializer.
                if g.is_slot {
                    self.cx.tables.insert(d, None);
                    return None;
                }
                let stored = self.store_table(g);
                self.cx.tables.insert(d, stored);
                stored?
            }
        };
        Some((data, len))
    }

    /// The elements of table `g` as static data, with their number; `None` when `g` is not a
    /// table lowering stores (`table_read`).
    fn store_table(&mut self, g: &hir::GlobalDef) -> Option<(vir::StaticId, usize)> {
        if let hir::ExprKind::ConstArray(c) = &g.init.kind {
            if let Some(stored) = self.store_const_array(c, g.ty) {
                return Some(stored);
            }
        }
        let unpacked = g.init.unpacked();
        let hir::ExprKind::ArrayLit(es) = &unpacked.kind else {
            return None;
        };
        if es.is_empty() {
            return None;
        }
        let elem = self.elem_ty(g.ty);
        let et = self.cx.ty(elem);
        let size = et.scalar_size().filter(|_| et != Ty::Ptr)? as usize;
        let mut bytes = Vec::with_capacity(es.len() * size);
        for e in es {
            match self.table_const(e, et, 0)? {
                Const::Int(v) => bytes.extend_from_slice(&v.to_le_bytes()[..size]),
                Const::Float(v) if et == Ty::F32 => {
                    bytes.extend_from_slice(&(v as f32).to_le_bytes())
                }
                Const::Float(v) => bytes.extend_from_slice(&v.to_le_bytes()),
                Const::Bool(b) => bytes.push(b as u8),
                _ => return None,
            }
        }
        Some((self.cx.static_bytes(bytes, size as u32), es.len()))
    }

    /// [`Self::store_table`] for a packed array of number literals (#3457): its bytes straight
    /// from the values, folded as [`Self::table_const`] folds each literal, without an
    /// expression per element. `None` (for the general path to decide) for an `f16`/`bf16`
    /// element, whose literals round, or a value `table_const` would not fold.
    fn store_const_array(
        &mut self,
        c: &hir::ConstArray,
        ty: hir::TyId,
    ) -> Option<(vir::StaticId, usize)> {
        if c.values.is_empty() || self.half_of(c.elem).is_some() {
            return None;
        }
        let elem = self.elem_ty(ty);
        let et = self.cx.ty(elem);
        let size = et.scalar_size().filter(|_| et != Ty::Ptr)? as usize;
        let at = self.vty(c.elem);
        let mut bytes = Vec::with_capacity(c.values.len() * size);
        for v in &c.values {
            let k = match (v.float, at.is_float()) {
                (false, true) => Const::Float(v.bits as f64),
                (false, false) if at.is_int() => {
                    Const::Int(super::operand::wrap_int(i128::from(v.bits), at))
                }
                (true, true) => Const::Float(f64::from_bits(v.bits)),
                _ => return None,
            };
            match k {
                Const::Int(n) => bytes.extend_from_slice(&n.to_le_bytes()[..size]),
                Const::Float(f) if et == Ty::F32 => {
                    bytes.extend_from_slice(&(f as f32).to_le_bytes())
                }
                Const::Float(f) => bytes.extend_from_slice(&f.to_le_bytes()),
                _ => return None,
            }
        }
        Some((self.cx.static_bytes(bytes, size as u32), c.values.len()))
    }

    /// The value of table element `e` as a constant of type `to`: a number or boolean literal,
    /// negated or cast (with `as` semantics), or a module constant that is one. `None` for
    /// anything else (sema's `table_elem` accepts exactly what this folds).
    pub(super) fn table_const(&mut self, e: &hir::Expr, to: Ty, depth: u32) -> Option<Const> {
        // An `f16`/`bf16` element (#2049): its rounded bits, into a table of the type.
        if let Some(h) = self.half_of(e.ty) {
            let bits = self.table_half(e, h, depth)?;
            return (to == Ty::U16).then_some(Const::Int(i128::from(bits)));
        }
        let at = self.vty(e.ty);
        let v = match &e.kind {
            hir::ExprKind::Lit(hir::Lit::Int(n)) if at.is_float() => Const::Float(*n as f64),
            hir::ExprKind::Lit(hir::Lit::Int(n)) if at.is_int() => {
                Const::Int(super::operand::wrap_int(*n as i128, at))
            }
            hir::ExprKind::Lit(hir::Lit::Float(f)) if at.is_float() => Const::Float(*f),
            hir::ExprKind::Lit(hir::Lit::Bool(b)) => Const::Bool(*b),
            hir::ExprKind::Unary {
                op: hir::UnOp::Neg,
                expr,
            } => match self.table_const(expr, at, depth)? {
                Const::Int(v) => Const::Int(super::operand::wrap_int(-v, at)),
                Const::Float(v) => Const::Float(-v),
                _ => return None,
            },
            // From an `f16`/`bf16`: its value, widened from its bits (exact).
            hir::ExprKind::Cast(inner) if self.half_of(inner.ty).is_some() => {
                let h = self.half_of(inner.ty)?;
                let bits = self.table_half(inner, h, depth)?;
                let w = match h {
                    FloatTy::F16 => crate::math::half::f16_to_f32(bits),
                    _ => crate::math::half::bf16_to_f32(bits),
                };
                Const::Float(f64::from(f32::from_bits(w)))
            }
            hir::ExprKind::Cast(inner) => {
                let from = self.vty(inner.ty);
                self.table_const(inner, from, depth)?
            }
            hir::ExprKind::Global(d) if depth < 16 => {
                let hir::Def::Global(g) = self.cx.hir.def(*d) else {
                    return None;
                };
                let from = self.vty(g.ty);
                match g.c_const {
                    // Its value, or (no value on this target) no table: its read is the error.
                    Some(c) => match c.value? {
                        hir::CValue::Int(v) => Const::Int(v),
                        hir::CValue::Float(v) => Const::Float(v),
                        hir::CValue::Bool(v) => Const::Bool(v),
                        hir::CValue::Address(_) => return None,
                    },
                    None if g.c_global.is_some() => return None,
                    None => self.table_const(&g.init, from, depth + 1)?,
                }
            }
            _ => return None,
        };
        // To the type asked for, as a cast does it.
        Some(match v {
            Const::Int(v) if to.is_int() => Const::Int(super::operand::wrap_int(v, to)),
            // Through `f64`, as `cast_to` folds it: the `f32` is rounded from that.
            Const::Int(v) if to == Ty::F32 => Const::Float(v as f64 as f32 as f64),
            Const::Int(v) if to.is_float() => Const::Float(v as f64),
            Const::Bool(b) if to.is_int() => Const::Int(b as i128),
            Const::Float(v) if to == Ty::F32 => Const::Float(v as f32 as f64),
            Const::Float(_) if to.is_float() => v,
            Const::Float(v) if to.is_int() => Const::Int(float_to_int(v, to)),
            Const::Bool(_) if to == Ty::Bool => v,
            _ => return None,
        })
    }
}

/// `v as to` for an integer type `to`: toward zero, saturating at the type's range, NaN is 0
/// (Rust's `as`, which is the language's).
fn float_to_int(v: f64, to: Ty) -> i128 {
    let bits = to.scalar_size().unwrap_or(8) * 8;
    let (lo, hi) = if to.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v.is_nan() {
        0
    } else {
        (v as i128).clamp(lo, hi)
    }
}
