//! Parameter attributes of user functions (vir.rs invariant 9), derived from each param's
//! `PassMode` and relying on sema's exclusivity rule (docs/reference/memory.md "Exclusive
//! access"): during a call, memory reachable through a `BorrowMut` param is reachable through
//! no other param, and `Borrow` params never alias a `BorrowMut` or `Owned` one.
//!
//! - `BorrowMut` (inferred modified) aggregate or class object → `noalias`, unless the type is
//!   counted or borrowed inside counted objects (semantics stage 2: other references may reach
//!   the value, `Cx::unique_refs`);
//! - `Borrow` aggregate or class object → `readonly` (again only for unique types), unless the
//!   value holds a `Mutex` inline
//!   (`m.with(...)` writes its lock word through a shared borrow) or the body writes the param
//!   (`LocalDef::mutable`: closure params, params handed to function values). Not `noalias`:
//!   two `Borrow` params may point to the same value;
//! - the out-pointer of an aggregate / `Result` return → `noalias` (always a fresh caller temp);
//! - every pointer to a value → `nonnull` + `dereferenceable(size)`;
//! - a poll function's frame → `nonnull` + `dereferenceable(frame size)` ([`Cx::poll_param_attrs`]).
//!
//! **Class objects** (every class-typed param, not only `this`; FINDINGS 8.1). A class value
//! is the pointer to its object, and the attributes describe the object's own bytes (its
//! fields; objects it points to are other memory). An object has exactly one owner (language
//! §9.1), so a second pointer to it exists only as a borrow the call site can see: the same
//! variable twice, an overlapping place (`a` / `a.child`), a `for...of` binding, a capture, or
//! a binding of an array element (`const x = xs[i]`, an alias of `xs`). Sema's exclusivity
//! check rejects each of these when the callee may modify the object, function values and
//! virtual / interface calls included (unknown callees count as modifying), there is no
//! mutable module state, and no function can return a reference into its arguments (moving
//! out of a field or element is an error). So during the call, the object behind a `BorrowMut`
//! param is reached through that param only (`noalias`), and nothing writes the object behind
//! a `Borrow` param (`readonly`). This is the argument that already covered `this`. With shared
//! references (semantics stage 2) it holds exactly for the classes the program never shares:
//! counted ones (and values borrowed inside counted objects) get neither attribute
//! (`Cx::unique_refs`).
//!
//! `Owned` / `Copy` aggregates get no `noalias`: calls through the borrow ABI (thunks, vtables)
//! may hand the callee the caller's own value instead of a copy.

use velt_sema::hir::{self, AdtKind, PassMode, TyId, TyKind};

use super::Cx;
use crate::vir::{AggId, ParamAttrs, Ty};

/// What a param's VIR value points to.
#[derive(Clone, Copy, Debug)]
pub(super) enum PtrParam {
    /// A value of this VIR type (aggregates, params passed by pointer).
    Value(Ty),
    /// A class-typed param (`this` or any other): the object.
    Object,
    /// Not a pointer to a value.
    NotPtr,
}

impl Cx<'_> {
    /// Attributes of a param of concrete type `ty` passed with `mode` (`None`: the callee may
    /// write through it whatever its mode, as `Mutex.with` callbacks do). `written`: the
    /// body writes the param (`LocalDef::mutable`), so even a borrowed one is not read-only.
    pub(super) fn param_attrs(
        &mut self,
        mode: Option<PassMode>,
        ty: TyId,
        shape: PtrParam,
        written: bool,
    ) -> ParamAttrs {
        let size = match shape {
            PtrParam::Value(t) => self.size_align(t).0,
            PtrParam::Object => {
                let obj = self.obj_agg(ty);
                self.aggs[obj.0 as usize].size
            }
            PtrParam::NotPtr => return ParamAttrs::default(),
        };
        let mut attrs = ParamAttrs {
            nonnull: true,
            dereferenceable: u64::from(size),
            ..ParamAttrs::default()
        };
        match mode {
            Some(PassMode::BorrowMut) => attrs.noalias = self.unique_refs(ty),
            // A counted object may be written through a share of the param (`const o = p;
            // o.x = 1`), a pointer derived from it: not read-only then.
            Some(PassMode::Borrow) => {
                attrs.readonly = !written && !self.holds_mutex(ty, 0) && self.unique_refs(ty)
            }
            _ => {}
        }
        attrs
    }

    /// Attributes of a poll function's `(state, cx)` params: the frame is a whole state of
    /// layout `state` (boxed or embedded in its parent's), never null. Not `noalias` here:
    /// pointers into the frame may outlive one poll (a child keeping its result buffer in a
    /// runtime future), so `velt_opt` adds it only where no frame address leaves the function.
    pub(super) fn poll_param_attrs(&self, state: AggId) -> Vec<ParamAttrs> {
        let frame = ParamAttrs {
            nonnull: true,
            dereferenceable: u64::from(self.aggs[state.0 as usize].size),
            ..ParamAttrs::default()
        };
        vec![frame, ParamAttrs::default()]
    }

    /// Attributes of the out-pointer of an aggregate return of type `ty` (VIR type).
    pub(super) fn out_ptr_attrs(&self, ty: Ty) -> ParamAttrs {
        ParamAttrs {
            noalias: true,
            nonnull: true,
            dereferenceable: u64::from(self.size_align(ty).0),
            readonly: false,
        }
    }

    /// Does a value of `ty` (a class: its object) contain a `Mutex` in its own memory? Values
    /// behind pointers (arrays, other objects, `shared`) don't count: writes to them are not
    /// writes through the param. `depth` guards against pathological nesting.
    pub(super) fn holds_mutex(&mut self, ty: TyId, depth: u32) -> bool {
        if depth > 32 {
            return true;
        }
        if let TyKind::Adt(d, _) = self.kind(ty) {
            if let hir::Def::Adt(a) = self.hir.def(d) {
                if a.kind != AdtKind::Class && self.type_name(ty) == "Mutex" {
                    return true;
                }
                if a.kind == AdtKind::Class && depth > 0 {
                    return false;
                }
            }
        }
        self.part_types(ty)
            .into_iter()
            .any(|p| self.holds_mutex(p, depth + 1))
    }
}
