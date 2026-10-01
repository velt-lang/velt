//! Prelude types the compiler knows by name: `Mutex<T>` (the lock-word struct behind
//! `new Mutex(x)` / `.with`), `JsonError` (thrown by `JSON.parse`), the dynamic JSON value
//! (`json::Value`), `Map` (not JSON-serializable), the shared-state receiver shapes, and the
//! `Comparable<T>` interface behind ordering operators on generic params.

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{DefId, IntTy, TyId, TyKind};

/// The method of the prelude's `Comparable<T>`; `a < b` on a bounded `T` calls it.
pub(crate) const COMPARE_TO: &str = "compareTo";

impl Ctx<'_> {
    /// `interface Comparable<T> { compareTo(other: T): i64 }` from the prelude.
    pub fn comparable_iface(&self) -> Option<DefId> {
        match self.prelude.get("Comparable") {
            Some(Item::Def(d)) if self.iface(*d).is_some() => Some(*d),
            _ => None,
        }
    }

    /// An ADT exported by the prelude under `name`.
    pub fn prelude_adt(&self, name: &str) -> Option<DefId> {
        match self.prelude.get(name) {
            Some(Item::Def(d)) if self.adt(*d).is_some() => Some(*d),
            _ => None,
        }
    }

    /// `struct Mutex<T> { lock: u64; value: T }` from the prelude.
    pub fn mutex_ty(&self) -> Option<DefId> {
        self.prelude_adt("Mutex")
    }

    /// `T` of a `Mutex<T>` or `shared<Mutex<T>>` receiver.
    pub fn mutex_value(&self, t: TyId) -> Option<TyId> {
        let m = match self.ty.kind(t) {
            TyKind::Shared(inner) => *inner,
            _ => t,
        };
        match self.ty.kind(m) {
            TyKind::Adt(d, args) if Some(*d) == self.mutex_ty() => args.first().copied(),
            _ => None,
        }
    }

    /// The integer type of a `shared<int>` receiver whose atomics the runtime provides (64-bit).
    pub fn atomic_int(&self, t: TyId) -> Option<TyId> {
        let TyKind::Shared(inner) = self.ty.kind(t) else {
            return None;
        };
        match self.ty.kind(*inner) {
            TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize) => Some(*inner),
            _ => None,
        }
    }

    /// Does a value of type `t` own a value with a `[Symbol.dispose]` drop hook or a promise (directly, or
    /// in a field, element or payload)? The compiler-generated deep `clone()` would copy such a
    /// resource handle (or the pointer to a future, which runs and is freed once) instead of
    /// duplicating the resource, so it is not available for these types.
    pub fn owns_resource(&mut self, t: TyId) -> bool {
        self.owns_resource_depth(t, 0)
    }

    fn owns_resource_depth(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            // A shared value's clone only bumps the reference count.
            TyKind::Shared(_) => return false,
            TyKind::Promise(..) => return true,
            // A function value owns its captures, not its parameter or result types.
            TyKind::FnPtr { .. } => return false,
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) if a.has_dispose => return true,
                    DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    DefInfo::Enum(e) => e
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter().copied())
                        .collect(),
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.ty.subst(f, &args)).collect()
            }
            k => crate::types::children(&k),
        };
        parts
            .into_iter()
            .any(|p| self.owns_resource_depth(p, depth + 1))
    }

    /// Is `e` a read of a `Record` value (`r[k]`: a call of `Record.__get` / `__at`)? It
    /// yields a copy, so modifying it is an error.
    pub fn is_record_read(&self, e: &crate::hir::Expr) -> bool {
        let crate::hir::ExprKind::Call {
            callee: crate::hir::Callee::Def(d, _),
            ..
        } = &e.kind
        else {
            return false;
        };
        if self.prelude_adt("Record").is_none() {
            return false;
        }
        let name = &self.fn_info(*d).name;
        name.starts_with("std/prelude/")
            && (name.ends_with("::Record.__get") || name.ends_with("::Record.__at"))
    }

    /// The dynamic JSON value class (`JsonValue` = std/prelude/json's private `Value`),
    /// recognized by name like the lowering's JSON glue does.
    pub fn is_json_value(&self, d: DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| a.qual_name.ends_with("json::Value"))
    }

    /// `JsonError` (prelude class), the type `JSON.parse` throws.
    pub fn json_error_ty(&mut self) -> Option<TyId> {
        let d = self.prelude_adt("JsonError")?;
        Some(self.ty.intern(TyKind::Adt(d, vec![])))
    }
}
