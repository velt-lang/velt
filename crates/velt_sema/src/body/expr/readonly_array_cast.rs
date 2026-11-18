//! Conversions with read-only arrays (#3061). `readonly T[]` (`ReadonlyArray<T>`) is a twin of
//! `T[]` (`Types::readonly_array`): the same array, which no code writes through.
//!
//! - `T[]` where `readonly T[]` is expected is the same array (an `Upcast`, a no-op once the twin
//!   is erased before lowering).
//! - An array of a narrower element type where `readonly U[]` is expected (`number[]` as
//!   `readonly unknown[]`): a fresh one converts as `U[]` does; an existing one is a view of that
//!   one array (#1485, `xs as U[]`), so a later write through the original shows through it, as in
//!   Node, and nothing writes through the view.
//! - `readonly T[]` where a mutable array is expected is refused, as TypeScript's TS4104 (an
//!   assignment), TS2322 (a return) and TS2345 (an argument) do: code could write through it.

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

/// What [`FnCx::readonly_array_value`] made of a value.
pub(super) enum ReadonlyCast {
    /// No read-only array involved: the other conversions apply.
    Other(hir::Expr),
    /// Converted.
    Done(hir::Expr),
    /// Refused: a read-only array where a mutable one is expected.
    Refused(hir::Expr),
}

impl FnCx<'_, '_> {
    /// `h` where `exp` is expected, when either is (or holds, as `| null`) a read-only array.
    pub(super) fn readonly_array_value(&mut self, h: hir::Expr, exp: TyId) -> ReadonlyCast {
        let core = self.cx.ty.core(exp).unwrap_or(exp);
        let from_ro = self.cx.ty.is_readonly_array(h.ty);
        let to_ro = self.cx.ty.is_readonly_array(core);
        // Neither involved, or already the expected read-only array, which only the usual wrapping
        // into `| null` / `| undefined` is left to do.
        if (!from_ro && !to_ro) || h.ty == core {
            return ReadonlyCast::Other(h);
        }
        let to_array = matches!(self.cx.ty.kind(core), TyKind::Array(_));
        if from_ro && to_array && !to_ro {
            return ReadonlyCast::Refused(h);
        }
        if from_ro && !to_array {
            // A union with a read-only array member takes it as that member (the union's own
            // conversion picks it, as it does any member).
            if self.has_readonly_array(exp) {
                return ReadonlyCast::Other(h);
            }
            // To `any`, `unknown` or another type: the array as itself, unless only a mutable
            // array is among what is expected.
            if self.has_mutable_array(exp) {
                return ReadonlyCast::Refused(h);
            }
            let mut h = h;
            h.ty = self.cx.ty.mutable_twin(h.ty);
            return ReadonlyCast::Other(h);
        }
        // `exp` (or its payload) is `readonly U[]`.
        let TyKind::Array(u) = *self.cx.ty.kind(core) else {
            return ReadonlyCast::Other(h);
        };
        let TyKind::Array(t) = *self.cx.ty.kind(h.ty) else {
            return ReadonlyCast::Other(h);
        };
        let mut h = h;
        // A read-only source is the same array as its mutable twin here: nothing writes through
        // what this makes either.
        h.ty = self.cx.ty.mutable_twin(h.ty);
        let wide = self.cx.ty.array(u);
        let as_wide = match t == u {
            true => Ok(h),
            false => match self.try_coerce(h, wide) {
                Ok(c) => Ok(c),
                // An existing array of a narrower element type: a view of it.
                Err(h) if self.converts_to(t, u) => self.through_any(h, wide),
                Err(h) => Err(h),
            },
        };
        // An element type that doesn't convert: a mismatch, as for `U[]`.
        let c = match as_wide {
            Ok(c) => c,
            Err(h) => return ReadonlyCast::Refused(h),
        };
        // The same array: no node at all, as the twin is `U[]` for lowering (a C call's view
        // argument stays the `Subarray` it looks for).
        let mut ro = c;
        ro.ty = core;
        match core == exp {
            true => ReadonlyCast::Done(ro),
            false => match self.try_coerce(ro, exp) {
                Ok(e) => ReadonlyCast::Done(e),
                Err(e) => ReadonlyCast::Other(e),
            },
        }
    }

    /// The checked operand `h` of `x as T`, where `T` holds a mutable array: a cast is how
    /// TypeScript code gets past `readonly`, so a read-only array (or one or `undefined`) is its
    /// array from here on (#3061). It is the same array: a write through the cast changes it, as
    /// in Node. (An array view of a narrower array, `number[]` seen as `readonly unknown[]`,
    /// stops a wider element with the view's run-time `TypeError`, as `xs as U[]` does.) Only the
    /// cast's own conversion is affected: the operand's own expressions are checked already.
    pub(super) fn readonly_cast_operand(&mut self, mut h: hir::Expr, target: TyId) -> hir::Expr {
        if !self.has_mutable_array(target) {
            return h;
        }
        let ty = &mut self.cx.ty;
        if ty.is_readonly_array(h.ty) {
            h.ty = ty.mutable_twin(h.ty);
        } else if let Some(p) = ty.opt_payload(h.ty).filter(|&p| ty.is_readonly_array(p)) {
            let n = ty.nothing_of(h.ty).expect("ICE: an option has its nothing");
            let plain = ty.mutable_twin(p);
            h.ty = ty.intern(TyKind::Option(plain, n));
        }
        h
    }

    /// Operand `h` of `===` or `!==` with a read-only array as its array (or one or nothing):
    /// comparing identities writes nothing, and the other operand may be the mutable array.
    pub(super) fn compared_array(&mut self, mut h: hir::Expr) -> hir::Expr {
        let ty = &mut self.cx.ty;
        if !ty.has_readonly_arrays() {
            return h;
        }
        if ty.is_readonly_array(h.ty) {
            h.ty = ty.mutable_twin(h.ty);
        } else if let Some(p) = ty.opt_payload(h.ty).filter(|&p| ty.is_readonly_array(p)) {
            let n = ty.nothing_of(h.ty).expect("ICE: an option has its nothing");
            let plain = ty.mutable_twin(p);
            h.ty = ty.intern(TyKind::Option(plain, n));
        }
        h
    }

    /// Does `t` have a read-only array type among the types it may be (its payload, its union's
    /// members)?
    fn has_readonly_array(&mut self, t: TyId) -> bool {
        let t = self.cx.ty.core(t).unwrap_or(t);
        let members = self.cx.union_members(t).unwrap_or_else(|| vec![t]);
        members.into_iter().any(|m| self.cx.ty.is_readonly_array(m))
    }

    /// Does `t` have a mutable array type among the types it may be (itself, its payload, its
    /// union's members)?
    fn has_mutable_array(&mut self, t: TyId) -> bool {
        let t = self.cx.ty.core(t).unwrap_or(t);
        let members = self.cx.union_members(t).unwrap_or_else(|| vec![t]);
        members.into_iter().any(|m| {
            matches!(self.cx.ty.kind(m), TyKind::Array(_)) && !self.cx.ty.is_readonly_array(m)
        })
    }
}
