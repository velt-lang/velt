//! The class hierarchy as the checking context sees it.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId, TyId, TyKind};

/// The classes extending each class, directly or not, in definition order, with how many
/// definitions they cover (a later unit adds its own classes after them).
pub(crate) type SubclassMap = (FxHashMap<DefId, Vec<DefId>>, usize);

impl Ctx<'_> {
    /// Is `t` a class instance type? Returns (class def, type args).
    pub fn class_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Base class type of class instance type `t` (with `t`'s type args substituted).
    pub fn base_of(&mut self, t: TyId) -> Option<TyId> {
        let (d, args) = self.class_of(t)?;
        let base = self.adt(d)?.base?;
        Some(self.subst(base, &args))
    }

    /// Is class `sub` class `sup` or one of its (transitive) subclasses?
    pub fn class_extends(&self, sub: DefId, sup: DefId) -> bool {
        let mut cur = Some(sub);
        for _ in 0..64 {
            match cur {
                Some(d) if d == sup => return true,
                Some(d) => {
                    cur = self
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.class_of(b))
                        .map(|(d, _)| d)
                }
                None => return false,
            }
        }
        false
    }

    /// The classes extending each class, directly or not, in definition order: built once per
    /// check, when every class is known. By units, the app's stage starts from std's unit's map
    /// (which std's stage keeps) and adds its own classes.
    pub(crate) fn subclass_map(&mut self) -> &FxHashMap<DefId, Vec<DefId>> {
        if self.subclasses.is_none() {
            let (mut map, from) = match (self.units_stage, &self.unit_sums.subclasses) {
                (crate::units::Stage::App, Some((m, n))) => (m.clone(), *n),
                _ => (FxHashMap::default(), 0),
            };
            let base_of = |cx: &Ctx, c: DefId| {
                (cx.adt(c).and_then(|a| a.base))
                    .and_then(|b| cx.class_of(b))
                    .map(|(b, _)| b)
            };
            for s in (from as u32..self.info.len() as u32).map(DefId) {
                let mut base = base_of(self, s);
                // Bounded like `Ctx::class_extends`.
                for _ in 0..64 {
                    let Some(b) = base else { break };
                    map.entry(b).or_default().push(s);
                    base = base_of(self, b);
                }
            }
            if self.units_stage == crate::units::Stage::Std {
                self.unit_sums.subclasses = Some((map.clone(), self.info.len()));
            }
            self.subclasses = Some(map);
        }
        self.subclasses
            .as_ref()
            .expect("ICE: the subclass map is built")
    }

    /// The classes extending class `d`, directly or not, in definition order.
    pub(crate) fn subclasses_of(&mut self, d: DefId) -> Vec<DefId> {
        self.subclass_map().get(&d).cloned().unwrap_or_default()
    }
}
