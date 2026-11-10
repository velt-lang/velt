//! The names a module's top-level declarations bind.

use velt_syntax::ast::{self, Item, ItemKind};

/// The name a top-level declaration binds.
pub(crate) fn declared_name(item: &Item) -> Option<&String> {
    Some(match &item.kind {
        ItemKind::Function(f) => &f.sig.name.name,
        // A decorated `declare function` is std's intrinsic (`@intrinsic`).
        ItemKind::ExternFn(s) if !s.decorators.is_empty() || s.is_decorator || s.host.is_some() => {
            &s.name.name
        }
        ItemKind::Struct(t) | ItemKind::Class(t) => &t.name.name,
        ItemKind::Interface(i) => &i.name.name,
        ItemKind::Enum(e) => &e.name.name,
        ItemKind::TypeAlias(t) => &t.name.name,
        ItemKind::Var(v) => match &v.pattern.kind {
            ast::PatternKind::Ident(name) => &name.name,
            _ => return None,
        },
        ItemKind::Extend(_)
        | ItemKind::Import(_)
        | ItemKind::ExternFn(_)
        | ItemKind::Augment(_)
        | ItemKind::Namespace(_) => return None,
    })
}
