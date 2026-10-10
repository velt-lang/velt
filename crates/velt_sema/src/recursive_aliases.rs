//! Recursive type aliases of object types (#654): `type Tree = { kids: Tree[] } & { v: number }`
//! and `type Tree = { kids: Tree[]; v: number }`. An alias is expanded where it is used, so one
//! that names itself would expand without end; an interface is a definition that its own fields
//! can name. Such an alias, whose body is an object type or an intersection of object types
//! written out (`{ … } & { … }`) with no field named twice, is checked as the field-only
//! interface with the same fields in the same order (`collect::field_only`): an object type
//! like the alias's, with the same layout and printing, the same name in messages, and the same
//! generics. Any other alias naming itself stays an error (`resolve::expand_alias`). The syntax
//! is rewritten before collection, like `generic_arrows`.

use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their recursive object type aliases rewritten into interfaces, or `None`
/// when no module has one (the common case: nothing is copied).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let any = modules
        .iter()
        .any(|m| m.ast.items.iter().any(|i| as_interface(i).is_some()));
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
                    .map(|i| as_interface(i).unwrap_or_else(|| i.clone()))
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

/// `item` as an interface, if it is a recursive alias of written-out object types.
fn as_interface(item: &ast::Item) -> Option<ast::Item> {
    let ast::ItemKind::TypeAlias(a) = &item.kind else {
        return None;
    };
    let parts: Vec<&ast::TypeExpr> = match &a.ty.kind {
        ast::TypeExprKind::Intersection(ps) => ps.iter().collect(),
        ast::TypeExprKind::Object(_) => vec![&a.ty],
        _ => return None,
    };
    let mut fields: Vec<ast::Field> = vec![];
    for p in parts {
        let ast::TypeExprKind::Object(fs) = &p.kind else {
            return None;
        };
        for f in fs {
            if fields.iter().any(|g| g.name.name == f.name.name) {
                return None;
            }
            fields.push(interface_field(f));
        }
    }
    if fields.is_empty() || !names(&a.ty, &a.name.name) {
        return None;
    }
    let decl = ast::InterfaceDecl {
        name: a.name.clone(),
        generics: a.generics.clone(),
        extends: vec![],
        fields,
        methods: vec![],
    };
    Some(ast::Item {
        kind: ast::ItemKind::Interface(decl),
        exported: item.exported,
        span: item.span,
    })
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
fn names(t: &ast::TypeExpr, name: &str) -> bool {
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
        } => {
            params.iter().any(|x| names(x, name))
                || names(ret, name)
                || throws.as_deref().is_some_and(|x| names(x, name))
        }
        K::Indexed { object, key } => names(object, name) || names(key, name),
        K::Object(fs) => fs.iter().any(|f| names(&f.ty, name)),
        K::Predicate { ty, .. } => ty.as_deref().is_some_and(|x| names(x, name)),
        K::Literal(_) | K::Null | K::Void => false,
    }
}
