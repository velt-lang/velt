//! Which interfaces TypeScript's own types satisfy through std's extensions ([`Widened`]).
//!
//! Velt makes a type implement an interface with an extension: `extend string implements
//! InlineKey`, or structurally, by an extension defining the interface's methods (`compareTo`
//! on every number type makes it `Comparable`); `extend<T> T implements Settles` makes every
//! type implement it, and `extend<T extends Comparable<T>> T implements InlineKey` every
//! comparable one. The type package declares no extensions of TypeScript's types, so a bound on
//! such an interface takes those types in.

use std::collections::{BTreeSet, HashMap};

use velt_common::FileId;
use velt_syntax::ast::{self, ItemKind, TypeExprKind};

use crate::ts::{Widened, NUMBERS};
use crate::Input;

/// What an extension extends.
enum Target {
    /// One of TypeScript's primitive types, as TypeScript names it.
    Primitive(String),
    /// Its own type parameter (`extend<T extends J> T`): every type satisfying the bounds.
    Every(Vec<String>),
}

struct Extension {
    target: Target,
    implements: Vec<String>,
    methods: Vec<String>,
}

/// What satisfies an interface beyond what declares it.
#[derive(Clone, PartialEq)]
enum Satisfied {
    All,
    Types(BTreeSet<String>),
}

/// The [`Widened`] bounds of std's `inputs`.
pub(super) fn widened(inputs: &[Input]) -> Widened {
    // Each interface's methods and type-parameter count.
    let mut ifaces: HashMap<String, (Vec<String>, usize)> = HashMap::new();
    let mut exts = vec![];
    for input in inputs {
        let (module, _) = velt_syntax::parse_file(FileId(0), &input.source);
        for item in &module.items {
            match &item.kind {
                ItemKind::Interface(i) => {
                    let methods = i.methods.iter().map(|m| m.sig.name.name.clone()).collect();
                    ifaces.insert(i.name.name.clone(), (methods, i.generics.len()));
                }
                ItemKind::Extend(e) => exts.extend(extension(e)),
                _ => {}
            }
        }
    }
    // The methods extensions give each primitive.
    let mut primitive_methods: HashMap<String, BTreeSet<String>> = HashMap::new();
    for e in &exts {
        if let Target::Primitive(p) = &e.target {
            primitive_methods
                .entry(p.clone())
                .or_default()
                .extend(e.methods.iter().cloned());
        }
    }
    let mut sat: HashMap<String, Satisfied> = HashMap::new();
    let add = |sat: &mut HashMap<String, Satisfied>, iface: &str, ty: Option<String>| {
        let entry = sat
            .entry(iface.to_string())
            .or_insert_with(|| Satisfied::Types(BTreeSet::new()));
        match (entry, ty) {
            (Satisfied::All, _) => {}
            (entry, None) => *entry = Satisfied::All,
            (Satisfied::Types(set), Some(ty)) => {
                set.insert(ty);
            }
        }
    };
    // Structurally: a primitive whose extensions define every method of the interface.
    for (name, (methods, _)) in &ifaces {
        if methods.is_empty() {
            continue;
        }
        for (p, have) in &primitive_methods {
            if methods.iter().all(|m| have.contains(m)) {
                add(&mut sat, name, Some(p.clone()));
            }
        }
    }
    for e in &exts {
        if let Target::Primitive(p) = &e.target {
            for i in &e.implements {
                add(&mut sat, i, Some(p.clone()));
            }
        }
    }
    // Blanket extensions, to a fixed point (`T extends J` brings in what satisfies `J`, and `J`
    // itself, which classes declare).
    loop {
        let before: Vec<(String, bool, usize)> = snapshot(&sat);
        for e in &exts {
            let Target::Every(bounds) = &e.target else {
                continue;
            };
            for i in &e.implements {
                if bounds.is_empty() {
                    add(&mut sat, i, None);
                    continue;
                }
                // Every type satisfying all the bounds; with one bound, exactly what satisfies it.
                let [j] = &bounds[..] else {
                    continue;
                };
                let arity = ifaces.get(j).map_or(0, |(_, n)| *n);
                let any = vec!["any"; arity].join(", ");
                let named = match arity {
                    0 => j.clone(),
                    _ => format!("{j}<{any}>"),
                };
                add(&mut sat, i, Some(named));
                match sat.get(j).cloned() {
                    Some(Satisfied::All) => add(&mut sat, i, None),
                    Some(Satisfied::Types(types)) => {
                        for t in types {
                            add(&mut sat, i, Some(t));
                        }
                    }
                    None => {}
                }
            }
        }
        if snapshot(&sat) == before {
            break;
        }
    }
    sat.into_iter()
        .map(|(name, s)| {
            let widened = match s {
                Satisfied::All => None,
                Satisfied::Types(types) => Some(types.into_iter().collect()),
            };
            (name, widened)
        })
        .collect()
}

fn snapshot(sat: &HashMap<String, Satisfied>) -> Vec<(String, bool, usize)> {
    let mut out: Vec<(String, bool, usize)> = sat
        .iter()
        .map(|(k, v)| match v {
            Satisfied::All => (k.clone(), true, 0),
            Satisfied::Types(t) => (k.clone(), false, t.len()),
        })
        .collect();
    out.sort();
    out
}

/// An extension of a primitive or of its own type parameter; `None` for any other target.
fn extension(e: &ast::ExtendDecl) -> Option<Extension> {
    let TypeExprKind::Named { path, args } = &e.target.kind else {
        return None;
    };
    let [name] = &path[..] else {
        return None;
    };
    let name = name.name.as_str();
    let target = match name {
        "string" => Target::Primitive("string".into()),
        "bool" | "boolean" => Target::Primitive("boolean".into()),
        n if NUMBERS.contains(&n) => Target::Primitive("number".into()),
        _ => {
            let g = e
                .generics
                .iter()
                .find(|g| g.name.name == name && args.is_empty())?;
            Target::Every(g.bounds.iter().filter_map(first_name).collect())
        }
    };
    Some(Extension {
        target,
        implements: e.implements.iter().filter_map(first_name).collect(),
        methods: e
            .methods
            .iter()
            .map(|m| m.decl.sig.name.name.clone())
            .collect(),
    })
}

/// A named type's name (`Comparable` for `Comparable<T>`).
fn first_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        TypeExprKind::Named { path, .. } => path.last().map(|i| i.name.clone()),
        _ => None,
    }
}
