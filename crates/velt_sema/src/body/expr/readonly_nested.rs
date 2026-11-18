//! Read-only arrays inside other types (#3061): `Promise<number[]>` where
//! `Promise<readonly number[]>` is expected, `number[][]` as `readonly (readonly number[])[]`,
//! `{ xs: number[] }` as `{ xs: readonly number[] }`, `[number[], string]` as
//! `[readonly number[], string]`. The two are one type once read-only arrays are erased before
//! lowering (`crate::readonly`), so the value converts as itself, an `Upcast`, when every
//! difference is a mutable array where a read-only one is expected (read-only where mutable is
//! expected in a function's parameter), as TypeScript allows; the other way is refused, as
//! TS4104 is.

use rustc_hash::FxHashMap;

use super::readonly_array_cast::ReadonlyCast;
use crate::body::FnCx;
use crate::ctx::Ctx;
use crate::hir::{self, AdtKind, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` where `exp` is expected, when the two differ only in read-only arrays inside them.
    pub(super) fn readonly_nested_value(&mut self, h: hir::Expr, exp: TyId) -> ReadonlyCast {
        if !self.cx.ty.has_readonly_arrays() || !self.cx.erased_alike(h.ty, exp) {
            return ReadonlyCast::Other(h);
        }
        if !self.cx.readonly_widens(h.ty, exp, true, 0) {
            return ReadonlyCast::Refused(h);
        }
        let span = h.span;
        ReadonlyCast::Done(self.mk(hir::ExprKind::Upcast(Box::new(h)), exp, span))
    }
}

impl Ctx<'_> {
    /// Are `a` and `b` different types that are one type once read-only arrays are mutable (and
    /// object types holding them their twins, `Ctx::array_twins`)? Not `readonly` fields or a
    /// frozen object's type: those conversions are decided elsewhere (`unfreezes`).
    pub(crate) fn erased_alike(&mut self, a: TyId, b: TyId) -> bool {
        if a == b {
            return false;
        }
        // What function types inside them throw is compared too: one whose error type its target
        // doesn't carry would be called with the wrong error layout, so function types that
        // differ in it (`Map<string, (x: readonly T[]) => number>`, two sites inferred apart)
        // are left to the conversions that carry errors from site to site.
        let (ea, eb) = self.erased_pair(a, b);
        ea == eb
    }

    /// [`Self::erased_alike`] but for what function types throw: for the message of a refusal
    /// only (TS4104), which must not depend on how errors are inferred.
    pub(crate) fn erased_alike_but_errors(&mut self, a: TyId, b: TyId) -> bool {
        if a == b {
            return false;
        }
        let (ea, eb) = self.erased_pair(a, b);
        ea == eb || self.ty.without_error_types(ea) == self.ty.without_error_types(eb)
    }

    /// `a` and `b` with read-only arrays mutable and object types holding them their twins.
    fn erased_pair(&mut self, a: TyId, b: TyId) -> (TyId, TyId) {
        let mut twins: FxHashMap<_, _> = (self.array_twins.iter())
            .map(|(&d, &t)| (d, (t, None)))
            .collect();
        // A field-only interface's object type is the anonymous one of its fields (`P` and
        // `{ xs: readonly number[] }`), whose own twin has mutable arrays.
        for (&d, (t, template)) in &self.readonly_twins {
            if self.field_only_of.contains_key(&d) {
                let t = self.array_twins.get(t).copied().unwrap_or(*t);
                twins.insert(d, (t, template.clone()));
            }
        }
        let mut cache = FxHashMap::default();
        let ea = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a);
        let eb = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b);
        (ea, eb)
    }

    /// `t` with its read-only arrays mutable, also inside the object types it names (each the
    /// twin of its own, `Ctx::array_twins`: `{ inner: In }` where `In = { xs: readonly T[] }`).
    pub(crate) fn plain_arrays(&mut self, t: TyId) -> TyId {
        self.plain_arrays_at(t, 0)
    }

    fn plain_arrays_at(&mut self, t: TyId, depth: u32) -> TyId {
        if !self.ty.has_readonly_arrays() || depth > 32 {
            return t;
        }
        let t = self.ty.mutable_twin(t);
        let k = self.ty.kind(t).clone();
        let k = match k {
            TyKind::Adt(d, args) => {
                let d = self.array_twins.get(&d).copied().unwrap_or(d);
                let args = args
                    .iter()
                    .map(|a| self.plain_arrays_at(*a, depth + 1))
                    .collect();
                TyKind::Adt(d, args)
            }
            k => crate::types::map_children(&k, &mut |x| self.plain_arrays_at(x, depth + 1)),
        };
        self.ty.intern(k)
    }

    /// Does a class field of type `have` provide an interface field of type `want` that differs
    /// only in read-only arrays, a mutable one where `want` has a read-only one (`xs: number[]`
    /// for `xs: readonly number[]`, `number[][]` for `readonly (readonly number[])[]`)? The
    /// field's getter gives its value, an out position.
    pub(crate) fn readonly_field_fits(&mut self, have: TyId, want: TyId) -> bool {
        self.ty.has_readonly_arrays()
            && self.erased_alike(have, want)
            && self.readonly_widens(have, want, true, 0)
    }

    /// Is `d` an object type's def: anonymous, or a field-only interface's?
    fn is_object_def(&self, d: crate::hir::DefId) -> bool {
        self.adt(d).is_some_and(|x| x.kind == AdtKind::Anon) || self.field_only_of.contains_key(&d)
    }

    /// Does a value of `from` convert to `to` as itself, read-only arrays aside: every
    /// position where one is read-only and the other is not takes a mutable array for a
    /// read-only one (`out`: a position values come out of), or a read-only one for a
    /// mutable one in a function's parameter (`!out`)?
    pub(crate) fn readonly_widens(&mut self, from: TyId, to: TyId, out: bool, depth: u32) -> bool {
        if from == to {
            return true;
        }
        // Being compared already, further out: it holds unless something else fails
        // (coinductive, as for a recursive type's next node).
        if self.readonly_widening.contains(&(from, to, out)) {
            return true;
        }
        if depth > 64 {
            return false;
        }
        self.readonly_widening.push((from, to, out));
        let ok = self.readonly_widens_step(from, to, out, depth);
        self.readonly_widening.pop();
        ok
    }

    fn readonly_widens_step(&mut self, from: TyId, to: TyId, out: bool, depth: u32) -> bool {
        let (rf, rt) = (
            self.ty.is_readonly_array(from),
            self.ty.is_readonly_array(to),
        );
        if rf != rt && rf == out {
            return false;
        }
        let d = depth + 1;
        let all = |cx: &mut Self, xs: &[TyId], ys: &[TyId], out: bool| {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| cx.readonly_widens(*x, *y, out, d))
        };
        match (self.ty.kind(from).clone(), self.ty.kind(to).clone()) {
            (TyKind::Array(a), TyKind::Array(b))
            | (TyKind::Option(a, _), TyKind::Option(b, _))
            | (TyKind::Shared(a), TyKind::Shared(b)) => self.readonly_widens(a, b, out, d),
            (TyKind::Promise(a, x), TyKind::Promise(b, y)) => {
                self.readonly_widens(a, b, out, d) && self.readonly_widens(x, y, out, d)
            }
            (TyKind::Map(a, x), TyKind::Map(b, y)) => {
                self.readonly_widens(a, b, out, d) && self.readonly_widens(x, y, out, d)
            }
            (TyKind::Tuple(xs), TyKind::Tuple(ys)) => all(self, &xs, &ys, out),
            (
                TyKind::FnPtr {
                    params: p, ret: r, ..
                },
                TyKind::FnPtr {
                    params: q, ret: s, ..
                },
            ) => {
                // What they throw is not compared here: where it differs, `readonly_nested_value`
                // converts by the rules for function values' errors.
                all(self, &p, &q, !out) && self.readonly_widens(r, s, out, d)
            }
            // `Map`, `Set` or `Record` at other arguments: element by element, as TypeScript
            // does. A class's arguments may sit in a function's parameter (`cb: (x: T) => void`),
            // so they must be the same.
            (TyKind::Adt(a, xs), TyKind::Adt(b, ys)) if a == b && !self.is_object_def(a) => {
                let container = ["Map", "Set", "Record"]
                    .iter()
                    .any(|n| self.prelude_adt(n) == Some(a));
                container && all(self, &xs, &ys, out)
            }
            // Object types (anonymous, or a field-only interface's), also one at other arguments
            // (`Box<number[]>` as `Box<readonly number[]>`): field by field, by name, so an
            // argument in a function field's parameter goes the other way. Others that lowering
            // sees as one type have no fields to differ in.
            (TyKind::Adt(a, xs), TyKind::Adt(b, ys)) => {
                let fields = |cx: &Self, d| {
                    cx.adt(d).filter(|_| cx.is_object_def(d)).map(|x| {
                        (x.fields.iter())
                            .map(|f| (f.name.clone(), f.ty))
                            .collect::<Vec<_>>()
                    })
                };
                let (Some(fa), Some(fb)) = (fields(self, a), fields(self, b)) else {
                    return false;
                };
                fa.len() == fb.len()
                    && fa.iter().all(|(n, t)| {
                        let Some((_, u)) = fb.iter().find(|(m, _)| m == n) else {
                            return false;
                        };
                        let (t, u) = (self.subst(*t, &xs), self.subst(*u, &ys));
                        self.readonly_widens(t, u, out, d)
                    })
            }
            // An interface value at other arguments (`Holder<readonly T[]>` as `Holder<T[]>`),
            // or another kind: not one value of the other type.
            _ => false,
        }
    }
}
