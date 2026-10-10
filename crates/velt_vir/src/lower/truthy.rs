//! `Intrinsic::Truthy`: JavaScript truthiness of a value whose type is a type parameter
//! (`!x`, `if (x)` on a `T`), lowered per instantiation to the test sema writes for a value of
//! that type (sema's truthiness.rs): `false`, `null`, `0` and `-0` of every number type, `NaN`
//! and `""` are falsy; a nullable by its payload, a union by the member it holds, an enum by its
//! member's value; objects, arrays and functions are always truthy.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{cint, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `Truthy(a)`: a `Bool` operand.
    pub(super) fn truthy_intrinsic(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let p = self.place_of(v, t);
        let out = Place::local(self.temp(Ty::Bool));
        self.truthy_into(&out, &p, t);
        Operand::Copy(out)
    }

    /// Assign whether the value at `p` (of the concrete type `t`) is truthy to `out`.
    fn truthy_into(&mut self, out: &Place, p: &Place, t: TyId) {
        let v = match self.cx.kind(t) {
            TyKind::Bool => Operand::Copy(p.clone()),
            TyKind::Int(_) => {
                let vt = self.cx.ty(t);
                self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(BinOp::Ne, Operand::Copy(p.clone()), cint(0, vt)),
                )
            }
            TyKind::Float(_) => return self.float_truthy_into(out, p, t),
            TyKind::Str => {
                let n = self.str_len(Operand::Copy(p.clone()));
                self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, n, cint(0, Ty::U64)))
            }
            TyKind::Literal(l) => bool_const(lit_truthy(&l)),
            TyKind::Option(e) => return self.option_truthy_into(out, p, t, e),
            TyKind::Adt(..) if self.cx.is_union(t) => return self.union_truthy_into(out, p, t),
            TyKind::Adt(d, _) if self.is_enum(t) && self.cx.is_c_like_enum(d) => {
                return self.enum_truthy_into(out, p, d)
            }
            TyKind::Unit | TyKind::Never => bool_const(false),
            _ => bool_const(true),
        };
        self.assign(out.clone(), Rvalue::Use(v));
    }

    /// `x != 0 && x == x`: neither `0`, `-0` nor `NaN`.
    fn float_truthy_into(&mut self, out: &Place, p: &Place, t: TyId) {
        let vt = self.cx.ty(t);
        let x = Operand::Copy(p.clone());
        let zero = Operand::Const(Const::Float(0.0), vt);
        let nonzero = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, x.clone(), zero));
        let (test, falsy, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(nonzero, test, falsy);
        self.switch_to(test);
        self.assign(out.clone(), Rvalue::Binary(BinOp::Eq, x.clone(), x));
        self.goto(done);
        self.switch_to(falsy);
        self.assign(out.clone(), Rvalue::Use(bool_const(false)));
        self.goto(done);
        self.switch_to(done);
    }

    /// Not `null`, and the payload is truthy.
    fn option_truthy_into(&mut self, out: &Place, p: &Place, t: TyId, e: TyId) {
        let (some_bb, none_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        let some = self.option_is_some(p, t);
        self.branch(some, some_bb, none_bb);
        self.switch_to(none_bb);
        self.assign(out.clone(), Rvalue::Use(bool_const(false)));
        self.goto(done);
        self.switch_to(some_bb);
        let payload = self.some_payload(p, t);
        self.truthy_into(out, &payload, e);
        self.goto(done);
        self.switch_to(done);
    }

    /// The member the union holds: a literal member by its value, a value member by its test.
    fn union_truthy_into(&mut self, out: &Place, p: &Place, t: TyId) {
        self.assign(out.clone(), Rvalue::Use(bool_const(true)));
        self.for_each_variant(p, t, |lw, v, parts| match lw.variant_literal(t, v) {
            Some(l) => lw.assign(out.clone(), Rvalue::Use(bool_const(lit_truthy(&l)))),
            None => match parts.as_slice() {
                [(pp, pt)] => lw.truthy_into(out, pp, *pt),
                [] => lw.assign(out.clone(), Rvalue::Use(bool_const(false))),
                _ => {}
            },
        });
    }

    /// Falsy for a member whose value is `0` or `""` (a string enum holds its member's index,
    /// a numeric one its value).
    fn enum_truthy_into(&mut self, out: &Place, p: &Place, d: hir::DefId) {
        let variants = &self.cx.enum_def(d).variants;
        let mut falsy: Vec<i128> = variants
            .iter()
            .enumerate()
            .filter_map(|(i, v)| match &v.str_value {
                Some(s) => s.is_empty().then_some(i as i128),
                None => (v.discriminant == 0).then_some(0),
            })
            .collect();
        falsy.sort_unstable();
        falsy.dedup();
        self.assign(out.clone(), Rvalue::Use(bool_const(true)));
        if falsy.is_empty() {
            return;
        }
        let (falsy_bb, done) = (self.new_block(), self.new_block());
        let cases: Vec<(i128, vir::BlockId)> = falsy.into_iter().map(|c| (c, falsy_bb)).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(p.clone()),
            cases,
            default: done,
        });
        self.switch_to(falsy_bb);
        self.assign(out.clone(), Rvalue::Use(bool_const(false)));
        self.goto(done);
        self.switch_to(done);
    }
}

fn bool_const(b: bool) -> Operand {
    Operand::Const(Const::Bool(b), Ty::Bool)
}

/// Is the one value of a literal type truthy?
fn lit_truthy(v: &LitValue) -> bool {
    match v {
        LitValue::Str(s) => !s.is_empty(),
        LitValue::Int(_, n) => *n != 0,
        LitValue::Float(_, bits) => {
            let f = f64::from_bits(*bits);
            f != 0.0 && !f.is_nan()
        }
        LitValue::Bool(b) => *b,
    }
}
