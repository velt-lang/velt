//! How code outside a class creates one of its instances, for the notes of errors that need one
//! (`new C(...)`, a factory, or what to add): split out of `ctx.rs`.

use crate::ctx::Ctx;
use crate::hir::{DefId, TyKind};

impl Ctx<'_> {
    /// The visibility of the constructor `new C(...)` of class `d` runs: its own, or the one it
    /// inherits from the nearest base class declaring one (public without any).
    pub fn ctor_visibility(&self, d: DefId) -> velt_syntax::ast::CtorVisibility {
        self.adt(d)
            .and_then(|a| a.ctor)
            .and_then(|c| self.fn_info(c).owner)
            .and_then(|owner| self.adt(owner)?.decl)
            .map_or_else(Default::default, |decl| decl.ctor_visibility)
    }

    /// The public static methods of class `d` that return a `d` (its factories), as
    /// `` `C.of(...)` or `C.parse(...)` ``; `None` without any.
    pub fn factories(&self, d: DefId) -> Option<String> {
        let a = self.adt(d)?;
        let mut names: Vec<String> = (self.factory_names(d).into_iter())
            .map(|name| format!("`{}.{name}(...)`", a.name))
            .collect();
        let last = names.pop()?;
        Some(match names.is_empty() {
            true => last,
            false => format!("{} or {last}", names.join(", ")),
        })
    }

    /// The names of the public static methods of class `d` that return a `d`, sorted.
    pub fn factory_names(&self, d: DefId) -> Vec<String> {
        let Some(a) = self.adt(d) else {
            return vec![];
        };
        let mut names: Vec<String> = a
            .methods
            .iter()
            .filter(|(_, m)| m.is_static && !self.fn_info(m.def).is_private)
            .filter(|(_, m)| {
                matches!(self.ty.kind(self.fn_info(m.def).ret), TyKind::Adt(r, _) if *r == d)
            })
            .map(|(name, _)| name.strip_prefix("static ").unwrap_or(name).to_string())
            .collect();
        names.sort();
        names
    }

    /// A note saying how code outside class `d` creates one: `new`, its factories, or (for a
    /// class of the user's own with a private or protected constructor and no factory) adding
    /// a factory.
    pub fn creation_note(&self, d: DefId) -> String {
        use velt_syntax::ast::CtorVisibility;
        let Some(a) = self.adt(d) else {
            return String::new();
        };
        let name = &a.name;
        let visibility = self.ctor_visibility(d);
        if visibility == CtorVisibility::Public {
            return format!("create it with `new {name}(...)`");
        }
        if let Some(factories) = self.factories(d) {
            return format!("create it with {factories}");
        }
        match visibility {
            _ if self.scopes[a.module].is_std => {
                format!("`{name}` values come from the functions of the module that declares it")
            }
            CtorVisibility::Protected => {
                format!("add a static factory method to `{name}`, or construct a subclass")
            }
            _ => format!("add a static factory method to `{name}` and call that"),
        }
    }
}
