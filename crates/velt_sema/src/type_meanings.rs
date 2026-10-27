//! A name that is both a type and a value, in separate meanings as in TypeScript: `type guid =
//! string` alongside `const guid = …`, or `interface Point` alongside `function Point(…)`.
//!
//! A module scope keeps such a name's value meaning under the name itself (where every value
//! lookup finds it) and its type meaning under the name's *type key* ([`type_key`], the name
//! with [`TYPE_MARK`] appended, which no identifier contains). Export tables, `export *` and
//! namespace imports carry the type key like any other name, so the two meanings travel
//! together; a named import or re-export of `x` binds both, `import type { x }` only the type.
//! Type positions look the type key up first ([`Ctx::lookup_type_item_at`]).
//!
//! A class or an enum is both a type and a value already, and still clashes with another
//! declaration of its name; so does an interface with a class (TypeScript merges them, Velt
//! doesn't, #384).

use velt_common::Span;

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;

/// Appended to a name to form its type key; not a character any identifier contains.
pub(crate) const TYPE_MARK: char = '\u{1}';

/// The key under which a scope keeps the type meaning of `name` (when it has a value meaning
/// too).
pub(crate) fn type_key(name: &str) -> String {
    let mut key = String::with_capacity(name.len() + 1);
    key.push_str(name);
    key.push(TYPE_MARK);
    key
}

/// Appended to a name in an export table for the value meaning of a name exported only as a
/// type (`export type { x }`, or `export { x }` of an `import type`): importers bind it as
/// `import type` binds a name, so a value use is an error.
pub(crate) const TYPE_ONLY_MARK: char = '\u{2}';

/// The key of a value exported only as a type ([`TYPE_ONLY_MARK`]).
pub(crate) fn type_only_key(name: &str) -> String {
    let mut key = String::with_capacity(name.len() + 1);
    key.push_str(name);
    key.push(TYPE_ONLY_MARK);
    key
}

/// A type key or a type-only key ([`TYPE_MARK`], [`TYPE_ONLY_MARK`]): not a name to list.
pub(crate) fn is_marked_key(key: &str) -> bool {
    key.ends_with([TYPE_MARK, TYPE_ONLY_MARK])
}

/// The name a type key is for (`None` for any other key).
pub(crate) fn of_type_key(key: &str) -> Option<&str> {
    key.strip_suffix(TYPE_MARK)
}

/// Which meanings a declaration gives its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Meaning {
    /// A type alias or an interface.
    Type,
    /// A function or a module-level variable.
    Value,
    /// A class, struct or enum.
    Both,
}

impl Ctx<'_> {
    pub(crate) fn meaning(&self, item: Item) -> Meaning {
        match item {
            Item::Alias(_) => Meaning::Type,
            Item::Def(d) => match self.info[d.0 as usize] {
                DefInfo::Iface(_) => Meaning::Type,
                DefInfo::Fn(_) | DefInfo::Global(_) => Meaning::Value,
                DefInfo::Adt(_) | DefInfo::Enum(_) => Meaning::Both,
            },
        }
    }

    /// Whether `a` and `b` give one name a type meaning and a value meaning that TypeScript
    /// keeps apart (one is only a type, the other only a value).
    pub(crate) fn separate_meanings(&self, a: Item, b: Item) -> bool {
        matches!(
            (self.meaning(a), self.meaning(b)),
            (Meaning::Type, Meaning::Value) | (Meaning::Value, Meaning::Type)
        )
    }

    /// A nested item `it` named `ident`, declared in block `scope` of module `m`, next to the
    /// items of the block declared so far (`self.nested[from..]`): another declaration of its
    /// name is an error unless one is only a type and the other only a value, or both are
    /// interfaces (which merge), as at module level.
    pub(crate) fn nested_clash(
        &mut self,
        from: usize,
        (m, scope): (usize, Span),
        ident: &velt_syntax::ast::Ident,
        it: Item,
    ) {
        let iface = |cx: &Self, i: Item| matches!(i, Item::Def(d) if cx.iface(d).is_some());
        let clash = (self.nested[from..].iter())
            .filter(|n| n.module == m && n.lo == scope.lo && n.hi == scope.hi)
            .filter(|n| n.name == ident.name)
            .any(|n| {
                !self.separate_meanings(n.item, it) && !(iface(self, n.item) && iface(self, it))
            });
        if clash {
            self.err(
                format!("the name `{}` is defined multiple times", ident.name),
                ident.span,
            );
        }
    }

    /// A function or a variable `name` written where a type is expected (TypeScript's TS2749).
    pub(crate) fn value_as_type(&mut self, name: &str, at: Span) {
        let d = velt_common::Diagnostic::error(format!("`{name}` is a value, not a type"), at)
            .with_note(format!("its type is `typeof {name}`"));
        self.error(d);
    }

    /// [`Ctx::lookup_item_at`] when nested items of the name are visible (`nested`): the
    /// innermost that is a value, else a module-level or prelude item, else the innermost type
    /// (for the error).
    pub(crate) fn nested_value_item(
        &self,
        mut nested: Vec<&crate::collect::NestedItem>,
        module: usize,
        name: &str,
    ) -> Option<Item> {
        nested.sort_by_key(|n| n.hi - n.lo);
        let value = nested
            .iter()
            .find(|n| self.meaning(n.item) != Meaning::Type);
        match value {
            Some(n) => Some(n.item),
            None => self
                .lookup_item(module, name)
                .or_else(|| nested.first().map(|n| n.item)),
        }
    }

    /// The global value a module-level type `name` leaves visible: the prelude's value, or
    /// `Some(None)` for a built-in global the checker knows by name (`Set`, `Promise`).
    pub(crate) fn global_value(&self, name: &str) -> Option<Option<Item>> {
        let prelude = self.prelude.get(name).copied();
        if let Some(p) = prelude.filter(|&p| self.meaning(p) != Meaning::Type) {
            return Some(Some(p));
        }
        let builtin = crate::body::expr::NODE_GLOBALS
            .iter()
            .any(|(n, _)| *n == name);
        (prelude.is_none() && builtin).then_some(None)
    }

    /// Name lookup in a type position of `module` at `at`: the type meaning of the innermost
    /// nested item or module-level name that has one, then the prelude's. A value (a function,
    /// a variable) does not hide a type of the same name further out, as in TypeScript; when no
    /// type is found the innermost value is the answer, for the error.
    pub fn lookup_type_item_at(&self, module: usize, name: &str, at: Span) -> Option<Item> {
        let file = self.modules[module].file;
        let mut nested: Vec<_> = (self.nested.iter())
            .filter(|n| n.module == module && n.name == name && at.file == file)
            .filter(|n| n.lo <= at.lo && at.hi <= n.hi)
            .collect();
        let mut value = None;
        if !nested.is_empty() {
            nested.sort_by_key(|n| n.hi - n.lo);
            for n in nested {
                match self.meaning(n.item) {
                    Meaning::Value => {
                        value.get_or_insert(n.item);
                    }
                    _ => return Some(n.item),
                }
            }
        }
        let scope = &self.scopes[module];
        let key = (scope.type_keys || self.prelude_type_keys).then(|| type_key(name));
        let scopes = [
            (&scope.items, scope.type_keys),
            (&self.prelude, self.prelude_type_keys),
        ];
        for (items, keyed) in scopes {
            if let Some(&it) = key.as_ref().filter(|_| keyed).and_then(|k| items.get(k)) {
                return Some(it);
            }
            match items.get(name) {
                Some(&it) if self.meaning(it) != Meaning::Value => return Some(it),
                Some(&it) => {
                    value.get_or_insert(it);
                }
                None => {}
            }
        }
        value
    }

    /// [`Ctx::lookup_type_item_at`] for a path: `ns.T` of a namespace import or `N.T` of a
    /// namespace member, the type meaning first.
    pub fn lookup_type_path_at(
        &self,
        module: usize,
        path: &[velt_syntax::ast::Ident],
        at: Span,
    ) -> Option<Item> {
        match path {
            [name] => self.lookup_type_item_at(module, &name.name, at),
            _ => {
                if !self.scopes[module].type_keys {
                    return self.lookup_path_at(module, path, at);
                }
                let joined: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
                let key = type_key(&joined.join("."));
                match self.scopes[module].items.get(&key) {
                    Some(&it) => Some(it),
                    None => self.lookup_path_at(module, path, at),
                }
            }
        }
    }
}
