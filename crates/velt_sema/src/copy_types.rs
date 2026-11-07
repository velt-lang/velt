//! Which types are Copy (`Ctx::is_copy`): values duplicated bitwise, with no owner to track.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};

impl Ctx<'_> {
    /// `Mutex`, `RwLock`, `Atomic`, `ThreadLocal`: inline `struct`s that are places, not values.
    pub fn place_struct(&self, d: DefId) -> bool {
        [
            self.mutex_ty(),
            self.rwlock_ty(),
            self.atomic_ty(),
            self.thread_local_ty(),
        ]
        .contains(&Some(d))
    }

    /// Is `t` a type declared with `struct` (a value type, #641)?
    pub(crate) fn declared_struct(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => {
                matches!(&self.info[d.0 as usize],
                DefInfo::Adt(a) if a.kind == crate::hir::AdtKind::Struct && a.decl.is_some())
                    && !self.place_struct(*d)
            }
            _ => false,
        }
    }

    /// Ownership: can values of this type be duplicated bitwise?
    pub fn is_copy(&mut self, t: TyId) -> bool {
        self.is_copy_depth(t, 0, &mut None)
    }

    /// Is `t`, reached `depth` levels down, Copy? Past 32 levels nothing is, which stops a
    /// generic type that grows as it recurses. `seen` keeps the answers per type and level of
    /// this query, so types that share parts are not checked once per path.
    fn is_copy_depth(
        &mut self,
        t: TyId,
        depth: u32,
        seen: &mut Option<FxHashMap<(TyId, u32), bool>>,
    ) -> bool {
        if depth > 32 {
            return false;
        }
        if let Some(base) = self.brand_base(t) {
            return self.is_copy_depth(base, depth + 1, seen);
        }
        match self.ty.kind(t).clone() {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Symbol
            | TyKind::Simd(_)
            | TyKind::Literal(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error => true,
            TyKind::Tuple(ts) => self.parts_copy(t, depth, &ts, None, seen),
            TyKind::Option(x, _) => self.parts_copy(t, depth, &[x], None, seen),
            TyKind::Result(a, b) => self.parts_copy(t, depth, &[a, b], None, seen),
            // Copying a lock would duplicate its lock word.
            TyKind::Adt(d, _) if Some(d) == self.mutex_ty() || Some(d) == self.rwlock_ty() => false,
            TyKind::Adt(d, args) => {
                // Objects are references: never Copy. A declared `struct` is a value (#641):
                // Copy when every field is (a struct with a `[Symbol.dispose]` is an error); a C
                // struct (collect/c_struct.rs) always is, its fields all being Copy.
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) if a.decl.is_some_and(|t| t.c_struct) => {
                        a.fields.iter().map(|f| f.ty).collect()
                    }
                    DefInfo::Enum(e) => e
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter().copied())
                        .collect(),
                    DefInfo::Adt(a)
                        if a.kind == crate::hir::AdtKind::Struct
                            && a.decl.is_some()
                            && !a.has_dispose
                            && !self.place_struct(d) =>
                    {
                        a.fields.iter().map(|f| f.ty).collect()
                    }
                    _ => return false,
                };
                self.parts_copy(t, depth, &tys, Some(&args), seen)
            }
            _ => false,
        }
    }

    /// Are `parts` of `t` all Copy? `t` is at `depth`; an enum's payloads are substituted with
    /// its type arguments `args`. The first levels are checked without keeping answers: most
    /// types end there, and the paths to a level multiply only below them.
    fn parts_copy(
        &mut self,
        t: TyId,
        depth: u32,
        parts: &[TyId],
        args: Option<&[TyId]>,
        seen: &mut Option<FxHashMap<(TyId, u32), bool>>,
    ) -> bool {
        let keep = depth >= 3;
        if let Some(&b) = seen
            .as_ref()
            .filter(|_| keep)
            .and_then(|m| m.get(&(t, depth)))
        {
            return b;
        }
        let b = parts.iter().all(|&p| {
            let p = args.map_or(p, |args| self.subst(p, args));
            self.is_copy_depth(p, depth + 1, seen)
        });
        if keep {
            seen.get_or_insert_with(FxHashMap::default)
                .insert((t, depth), b);
        }
        b
    }
}
