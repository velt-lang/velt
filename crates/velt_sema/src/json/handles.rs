//! Which types keep handles in their private fields, which JSON must never carry or forge
//! (docs/std/encoding/json.md, "Private fields").

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnKind};
use crate::hir::DefId;

impl Ctx<'_> {
    /// Does type definition `d` keep runtime or native handles in its private fields, which
    /// JSON must never carry or forge? A std type does, and so does a type declared in a module
    /// that binds native code ([`Self::binds_native`]): a package's database handles are
    /// `private handle: u64` next to the native functions they are passed to.
    pub(crate) fn keeps_handles(&self, d: DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| self.scopes[a.module].is_std || self.binds_native(a.module))
    }

    /// Does module `m` declare or import a native function: a `declare function` of a native
    /// library (`velt_<package>__…`, docs/book/native-packages.md) or a `declare system
    /// function`? (Not an intrinsic, a generated function or another ambient declaration.)
    fn binds_native(&self, m: usize) -> bool {
        self.scopes[m].items.values().any(|item| {
            let Item::Def(f) = item else { return false };
            let Some(DefInfo::Fn(f)) = self.info.get(f.0 as usize) else {
                return false;
            };
            f.kind == FnKind::Extern
                && f.intrinsic.is_none()
                && (f.system.is_some() || f.name.starts_with("velt_"))
        })
    }
}
