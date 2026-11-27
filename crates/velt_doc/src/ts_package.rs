//! What the type package's declarations know beyond their own file: the bounds TypeScript's
//! own types satisfy through std's extensions ([`Widened`], computed by [`crate::dts`]), and the
//! default methods a class gets from the interfaces it implements ([`Defaults`]).
//!
//! A Velt interface method may have a body (`tryStream(): TryStream | null { return null; }`):
//! a class implementing the interface has the method without declaring it. TypeScript has no
//! such defaults, so a class declaration lists each one it does not declare itself, typed by the
//! interface (`tryStream: ByteStream["tryStream"];`): `implements` holds, and clients see the
//! method. The interface's member type keeps its generics and needs no name beyond the
//! interface, which the class's `implements` already refers to.

use std::collections::HashMap;

use velt_common::FileId;
use velt_syntax::ast::{self, ItemKind, TypeExprKind};

use crate::ts::{indented, TsPrinter};
use crate::Input;

/// What satisfies an interface beyond what declares it, through std's extensions (computed by
/// [`crate::dts`]): a bound on it (`K extends Comparable<K>`) widens to those of TypeScript's
/// types (`Comparable<K> | string | number | boolean`), or goes (`None`: every type does,
/// `extend<T> T implements Settles`). TypeScript sees no extensions of its own types, which
/// a type-only import must not add to the client's globals.
pub(crate) type Widened = HashMap<String, Option<Vec<String>>>;

/// A method of an interface with a body, or one of an interface it extends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefaultMember {
    pub(crate) name: String,
    pub(crate) getter: bool,
    pub(crate) setter: bool,
}

/// Each interface's default members (also those of the interfaces it extends), by its name.
pub(crate) type Defaults = HashMap<String, Vec<DefaultMember>>;

/// The [`Defaults`] of std's `inputs`.
pub(crate) fn defaults(inputs: &[Input]) -> Defaults {
    // Each interface's own default members and the names of the interfaces it extends.
    let mut own: HashMap<String, (Vec<DefaultMember>, Vec<String>)> = HashMap::new();
    for input in inputs {
        let (module, _) = velt_syntax::parse_file(FileId(0), &input.source);
        for item in &module.items {
            let ItemKind::Interface(i) = &item.kind else {
                continue;
            };
            let members = i
                .methods
                .iter()
                .filter(|m| m.body.is_some() && !m.sig.name.name.starts_with('['))
                .map(|m| DefaultMember {
                    name: m.sig.name.name.clone(),
                    getter: m.is_getter,
                    setter: m.is_setter,
                })
                .collect();
            let extends = i.extends.iter().filter_map(type_name).collect();
            own.insert(i.name.name.clone(), (members, extends));
        }
    }
    own.keys()
        .map(|name| {
            let mut out: Vec<DefaultMember> = vec![];
            let mut seen = vec![];
            let mut todo = vec![name.clone()];
            while let Some(n) = todo.pop() {
                if seen.contains(&n) {
                    continue;
                }
                let Some((members, extends)) = own.get(&n) else {
                    continue;
                };
                for m in members {
                    if !out.contains(m) {
                        out.push(m.clone());
                    }
                }
                todo.extend(extends.iter().cloned());
                seen.push(n);
            }
            (name.clone(), out)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

/// The last name of a named type (`ByteStream` of `velt.net.ByteStream<T>`).
fn type_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        TypeExprKind::Named { path, .. } => path.last().map(|i| i.name.clone()),
        _ => None,
    }
}

impl TsPrinter<'_> {
    /// The members class `t` has from the defaults of the interfaces it implements, those it
    /// does not declare itself.
    pub(crate) fn interface_defaults(&self, t: &ast::TypeDecl) -> Vec<String> {
        let declared = |name: &str| {
            t.fields.iter().any(|f| f.name.name == name)
                || t.methods.iter().any(|m| m.decl.sig.name.name == name)
        };
        let mut out: Vec<String> = vec![];
        let mut done: Vec<&DefaultMember> = vec![];
        for iface in &t.implements {
            let Some(members) = type_name(iface).and_then(|n| self.defaults.get(&n)) else {
                continue;
            };
            let ty = self.operand(iface);
            for m in members {
                if declared(&m.name) || done.contains(&m) {
                    continue;
                }
                done.push(m);
                let (n, of) = (&m.name, format!("{ty}[\"{}\"]", m.name));
                let text = match (m.getter, m.setter) {
                    (true, _) => format!("get {n}(): {of};"),
                    (_, true) => format!("set {n}(value: {of});"),
                    _ => format!("{n}: {of};"),
                };
                let doc = format!(
                    "/** The default of `{}`. */",
                    type_name(iface).unwrap_or_default()
                );
                out.push(indented(&doc, &text));
            }
        }
        out
    }
}
