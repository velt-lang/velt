//! Recursive type aliases of object types (#654): `type Tree = { kids: Tree[] } & { v: number }`
//! and `type Tree = { kids: Tree[]; v: number }`. An alias is expanded where it is used, so one
//! that names itself would expand without end; an interface is a definition that its own fields
//! can name. Such an alias, whose body is an object type or an intersection of object types
//! written out (`{ … } & { … }`) with no field named twice, is checked as the field-only
//! interface with the same fields in the same order (`collect::field_only`): an object type
//! like the alias's, with the same layout and printing, the same name in messages, and the same
//! generics. Any other alias naming itself stays an error (`resolve::expand_alias`). The syntax
//! is rewritten before collection, like `generic_arrows`. A part may also name a non-generic
//! alias of such a type in the same module (`type Tree = Base & { kids: Tree[] }`, #783), whose
//! fields stand where its name does.

use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their recursive object type aliases rewritten into interfaces, or `None`
/// when no module has one (the common case: nothing is copied).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let any = modules.iter().any(|m| {
        m.ast
            .items
            .iter()
            .any(|i| as_interface(i, &m.ast.items).is_some())
    });
    if !any {
        return None;
    }
    let modules = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: ast::Module {
                items: m
                    .ast
                    .items
                    .iter()
                    .map(|i| as_interface(i, &m.ast.items).unwrap_or_else(|| i.clone()))
                    .collect(),
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    Some(modules)
}

/// `item` as an interface, if it is a recursive alias of object types written out or named
/// (`items` are its module's items).
fn as_interface(item: &ast::Item, items: &[ast::Item]) -> Option<ast::Item> {
    let ast::ItemKind::TypeAlias(a) = &item.kind else {
        return None;
    };
    if !matches!(
        a.ty.kind,
        ast::TypeExprKind::Intersection(_) | ast::TypeExprKind::Object(_)
    ) {
        return None;
    }
    let mut parts = vec![];
    object_parts(&a.ty, items, &mut vec![a.name.name.as_str()], &mut parts)?;
    let mut fields: Vec<ast::Field> = vec![];
    for f in parts {
        if fields.iter().any(|g| g.name.name == f.name.name) {
            return None;
        }
        fields.push(interface_field(f));
    }
    if fields.is_empty() || !fields.iter().any(|f| names(&f.ty, &a.name.name)) {
        return None;
    }
    let decl = ast::InterfaceDecl {
        name: a.name.clone(),
        generics: a.generics.clone(),
        extends: vec![],
        fields,
        methods: vec![],
        call_sigs: vec![],
    };
    Some(ast::Item {
        kind: ast::ItemKind::Interface(decl),
        exported: item.exported,
        span: item.span,
    })
}

/// The fields of `t`, an object type, an intersection of them, or the name of a non-generic alias
/// of one in the same module (`Base` in `type Tree = Base & { kids: Tree[] }`, #783), in order;
/// `None` for any other type. `seen`: the aliases being expanded (an alias naming itself here
/// has no fields to give).
fn object_parts<'a>(
    t: &'a ast::TypeExpr,
    items: &'a [ast::Item],
    seen: &mut Vec<&'a str>,
    out: &mut Vec<&'a ast::ObjectTypeField>,
) -> Option<()> {
    match &t.kind {
        ast::TypeExprKind::Object(fs) => out.extend(fs),
        ast::TypeExprKind::Intersection(ps) => {
            for p in ps {
                object_parts(p, items, seen, out)?;
            }
        }
        ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
            let name = path[0].name.as_str();
            if seen.contains(&name) {
                return None;
            }
            let alias = items.iter().find_map(|i| match &i.kind {
                ast::ItemKind::TypeAlias(b) if b.name.name == name => Some(b),
                _ => None,
            })?;
            if !alias.generics.is_empty() {
                return None;
            }
            seen.push(name);
            object_parts(&alias.ty, items, seen, out)?;
            seen.pop();
        }
        _ => return None,
    }
    Some(())
}

/// An object type's field as an interface declares it (the type without the `| null` that the
/// parser adds to an optional field of an object type).
fn interface_field(f: &ast::ObjectTypeField) -> ast::Field {
    let ty = match f.optional {
        true => crate::resolve::written_type(&f.ty),
        false => f.ty.clone(),
    };
    ast::Field {
        name: f.name.clone(),
        ty,
        default: None,
        readonly: f.readonly,
        optional: f.optional,
        is_private: false,
        is_static: false,
        span: f.span,
    }
}

/// Does type `t` name `name` (a one-segment named type) anywhere?
pub(crate) fn names(t: &ast::TypeExpr, name: &str) -> bool {
    use ast::TypeExprKind as K;
    match &t.kind {
        K::Named { path, args } => {
            (path.len() == 1 && path[0].name == name) || args.iter().any(|a| names(a, name))
        }
        K::Array(e) => names(e, name),
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => ts.iter().any(|x| names(x, name)),
        K::Function {
            params,
            ret,
            throws,
            ..
        } => {
            params.iter().any(|x| names(x, name))
                || names(ret, name)
                || throws.as_deref().is_some_and(|x| names(x, name))
        }
        K::GenericFunction { ty, .. }
        | K::Construct(ty)
        | K::TupleRest(ty)
        | K::TupleOptional(ty) => names(ty, name),
        K::Indexed { object, key } => names(object, name) || names(key, name),
        K::Object(fs) => fs.iter().any(|f| names(&f.ty, name)),
        K::IndexSig {
            fields, key, value, ..
        } => {
            fields.iter().any(|f| names(&f.ty, name)) || names(&key.ty, name) || names(value, name)
        }
        K::Callable { sigs, members } => {
            names(&crate::call_sigs::lower(sigs, members, t.span), name)
        }
        K::KeyOf(t) => names(t, name),
        K::Conditional(c) => [&c.check, &c.extends, &c.then, &c.else_]
            .iter()
            .any(|x| names(x, name)),
        K::Mapped(m) => {
            names(&m.source, name)
                || m.name.as_ref().is_some_and(|x| names(x, name))
                || names(&m.value, name)
        }
        K::TemplateLiteral { types, .. } => types.iter().any(|x| names(x, name)),
        K::Infer(_) => false,
        K::Predicate { ty, .. } => ty.as_deref().is_some_and(|x| names(x, name)),
        K::Literal(_) | K::Null | K::Undefined | K::Void | K::Query(_) => false,
    }
}
