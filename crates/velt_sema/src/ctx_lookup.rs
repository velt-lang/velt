//! Name lookup at module level and at a source position.

use velt_common::Span;

use crate::ctx::{Ctx, Item};

impl Ctx<'_> {
    /// Name lookup at module level: own items and imports, then the prelude.
    /// A module-level name that is only a type does not hide a prelude value of its name
    /// (`type Map = …` next to `new Map()`, `crate::type_meanings`).
    pub fn lookup_item(&self, module: usize, name: &str) -> Option<Item> {
        let own = self.scopes[module].items.get(name).copied();
        let Some(it) = own else {
            return self.prelude_or_hook(name);
        };
        match self.meaning(it) {
            crate::type_meanings::Meaning::Type => self.global_value(name).unwrap_or(own),
            _ => own,
        }
    }

    /// Name lookup at a source position of `module`: the innermost nested item whose block
    /// contains `at`, then module level.
    /// A nested type that is only a type (an interface, a type alias) does not hide a value of
    /// its name further out (`crate::type_meanings`).
    pub fn lookup_item_at(&self, module: usize, name: &str, at: Span) -> Option<Item> {
        let file = self.modules[module].file;
        let mut nested = self
            .nested
            .iter()
            .filter(|n| n.module == module && n.name == name && at.file == file)
            .filter(|n| n.lo <= at.lo && at.hi <= n.hi)
            .peekable();
        if nested.peek().is_none() {
            return self.lookup_item(module, name);
        }
        self.nested_value_item(nested.collect(), module, name)
    }
}
