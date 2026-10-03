//! Every export and member of `std/prelude` is classified, exactly once.

use std::collections::BTreeSet;
use std::path::Path;

use velt_common::FileId;
use velt_syntax::ast::{self, ItemKind as I, TypeExprKind as T};

use super::{TS_GLOBALS, TS_MEMBERS, VELT_GLOBALS, VELT_MEMBERS};

/// The prelude's exports and its `(owner, member)` pairs.
fn prelude() -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/prelude");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("std/prelude")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    files.sort();
    let (mut exports, mut members) = (BTreeSet::new(), BTreeSet::new());
    for file in files {
        let src = std::fs::read_to_string(&file).expect("read the prelude");
        let (module, diags) = velt_syntax::parse_file(FileId(0), &src);
        assert!(diags.is_empty(), "{}: {diags:?}", file.display());
        for item in &module.items {
            collect(item, &mut exports, &mut members);
        }
    }
    (exports, members)
}

fn collect(
    item: &ast::Item,
    exports: &mut BTreeSet<String>,
    members: &mut BTreeSet<(String, String)>,
) {
    let mut add = |owner: &str, name: &str| {
        if !name.starts_with('[') && !name.starts_with("__") {
            members.insert((owner.to_string(), name.to_string()));
        }
    };
    let name = match &item.kind {
        I::Function(f) => Some(&f.sig.name),
        I::Class(t) | I::Struct(t) if item.exported => {
            let owner = owner_of_class(&t.name.name);
            for f in t.fields.iter().filter(|f| !f.is_private) {
                add(owner, &f.name.name);
            }
            for m in t.methods.iter().filter(|m| !m.is_private) {
                add(owner, &m.decl.sig.name.name);
            }
            Some(&t.name)
        }
        I::Interface(i) if item.exported => {
            for f in &i.fields {
                add(&i.name.name, &f.name.name);
            }
            for m in &i.methods {
                add(&i.name.name, &m.sig.name.name);
            }
            Some(&i.name)
        }
        I::Extend(x) => {
            let owner = owner_of_extend(&x.target);
            for m in x.methods.iter().filter(|m| !m.is_private) {
                add(owner, &m.decl.sig.name.name);
            }
            None
        }
        I::Var(v) => match &v.pattern.kind {
            ast::PatternKind::Ident(id) => Some(id),
            _ => None,
        },
        I::TypeAlias(a) => Some(&a.name),
        I::Enum(e) => Some(&e.name),
        I::Class(_) | I::Struct(_) | I::Interface(_) | I::Import(_) | I::ExternFn(_) => None,
    };
    if let Some(name) = name.filter(|_| item.exported) {
        if !name.name.starts_with("__") {
            exports.insert(name.name.clone());
        }
    }
}

/// The owner the lint names a class's members by.
fn owner_of_class(name: &str) -> &str {
    match name {
        "NumberConstructor" => "Number",
        other => other,
    }
}

fn owner_of_extend(target: &ast::TypeExpr) -> &'static str {
    match &target.kind {
        T::Named { path, .. } => match path[0].name.as_str() {
            "Array" => "Array",
            "string" => "string",
            "bool" | "boolean" => "boolean",
            "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
            | "f32" | "f64" | "number" => "number",
            "Map" => "Map",
            "IterableIterator" => "IterableIterator",
            "IteratorObject" => "IteratorObject",
            "AsyncIterableIterator" => "AsyncIterableIterator",
            other => panic!("an `extend` of `{other}`: name the owner the lint uses for it"),
        },
        T::Union(_) => "nullable",
        other => panic!("an `extend` of {other:?}"),
    }
}

#[test]
fn every_prelude_export_is_classified_once() {
    let (exports, _) = prelude();
    let velt: BTreeSet<&str> = VELT_GLOBALS.iter().map(|(n, _)| *n).collect();
    let ts: BTreeSet<&str> = TS_GLOBALS.iter().copied().collect();
    let both: Vec<&&str> = ts.intersection(&velt).collect();
    assert!(both.is_empty(), "classified twice: {both:?}");
    let missing: Vec<&String> = exports
        .iter()
        .filter(|e| !ts.contains(e.as_str()) && !velt.contains(e.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "classify these prelude exports in typed/prelude.rs (TS_GLOBALS or VELT_GLOBALS): \
         {missing:?}"
    );
}

#[test]
fn every_prelude_member_is_classified_once() {
    let (_, members) = prelude();
    let pairs = |owner: &str, names: Vec<&str>| -> Vec<(String, String)> {
        names
            .into_iter()
            .map(|n| (owner.to_string(), n.to_string()))
            .collect()
    };
    let ts: BTreeSet<(String, String)> = TS_MEMBERS
        .iter()
        .flat_map(|(o, ns)| pairs(o, ns.to_vec()))
        .collect();
    let velt: BTreeSet<(String, String)> = VELT_MEMBERS
        .iter()
        .flat_map(|(o, ns)| pairs(o, ns.iter().map(|(n, _)| *n).collect()))
        .collect();
    let both: Vec<_> = ts.intersection(&velt).collect();
    assert!(both.is_empty(), "classified twice: {both:?}");
    let missing: Vec<String> = members
        .iter()
        .filter(|m| !ts.contains(*m) && !velt.contains(*m))
        .map(|(o, n)| format!("{o}.{n}"))
        .collect();
    assert!(
        missing.is_empty(),
        "classify these prelude members in typed/prelude.rs (TS_MEMBERS or VELT_MEMBERS): \
         {missing:?}"
    );
}
