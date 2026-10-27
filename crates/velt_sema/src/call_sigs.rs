//! Call and construct signatures in interfaces and object types (`interface F { (x: T): R }`,
//! `{ new (x: T): C; n: number }`). The syntax keeps them as written ([`ast::CallSig`]); they
//! get the type TypeScript gives them: one signature alone is that function type
//! (`(x: T) => R`, `new (x: T) => C`), more signatures (overloads) or other members the
//! intersection of all of them (`((x: T) => R) & { n: number }`).
//!
//! - An object type with signatures ([`ast::TypeExprKind::Callable`]) is resolved as that type
//!   ([`lower`], from `resolve`).
//! - A module-level interface with signatures is rewritten before collection, after interface
//!   merging, into the type alias of that type ([`rewrite`]); its bases are parts of the
//!   intersection and its methods fields of function type.
//! - [`check`] reports what is not supported in a signature, once per signature.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;
use velt_syntax::visit::{walk_module, Visit};

use crate::SourceModule;

/// The type of an object type with signatures `sigs` and the other members `members`.
pub(crate) fn lower(sigs: &[ast::CallSig], members: &ast::TypeExpr, span: Span) -> ast::TypeExpr {
    let rest = match &members.kind {
        ast::TypeExprKind::Object(fields) if fields.is_empty() => vec![],
        _ => vec![members.clone()],
    };
    callable_type(sigs, rest, span)
}

/// One signature alone, or the intersection of the signatures and `rest`.
fn callable_type(sigs: &[ast::CallSig], rest: Vec<ast::TypeExpr>, span: Span) -> ast::TypeExpr {
    // Errors about the whole type point at its first signature.
    let span = sigs.first().map_or(span, |s| s.span);
    let mut parts: Vec<ast::TypeExpr> = sigs.iter().map(sig_type).collect();
    parts.extend(rest);
    if parts.len() == 1 {
        return parts.pop().expect("ICE: one part");
    }
    ast::TypeExpr {
        kind: ast::TypeExprKind::Intersection(parts),
        span,
    }
}

/// The function type a signature declares: `(x: T) => R`, `<T>(x: T) => R`,
/// `new (x: T) => C` (a missing return type is `void`).
fn sig_type(s: &ast::CallSig) -> ast::TypeExpr {
    let ty = fn_type(&s.sig, s.span);
    match s.construct {
        true => ast::TypeExpr {
            kind: ast::TypeExprKind::Construct {
                ty: Box::new(ty),
                is_abstract: false,
            },
            span: s.span,
        },
        false => ty,
    }
}

/// `(x: T) => R` / `<T>(x: T) => R` for signature `sig` (at `span`).
fn fn_type(sig: &ast::FnSig, span: Span) -> ast::TypeExpr {
    let ret = sig.ret.clone().unwrap_or(ast::TypeExpr {
        kind: ast::TypeExprKind::Void,
        span: Span::new(span.file, span.hi, span.hi),
    });
    let ty = ast::TypeExpr {
        kind: ast::TypeExprKind::Function {
            rest: sig.params.last().is_some_and(|p| p.rest),
            params: sig.params.iter().map(|p| p.ty.clone()).collect(),
            ret: Box::new(ret),
            throws: sig.throws.clone().map(Box::new),
        },
        span,
    };
    match sig.generics.is_empty() {
        true => ty,
        false => ast::TypeExpr {
            kind: ast::TypeExprKind::GenericFunction {
                type_params: sig.generics.clone(),
                ty: Box::new(ty),
            },
            span,
        },
    }
}

/// The modules with every module-level interface that has signatures rewritten into a type
/// alias, or `None` when there is none; with the errors found.
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Option<Vec<SourceModule>>, Vec<Diagnostic>) {
    let fn_bases = function_names(modules);
    let callable = |i: &ast::Item| match &i.kind {
        ast::ItemKind::Interface(d) => {
            !d.call_sigs.is_empty() || d.extends.iter().any(|b| names_function(b, &fn_bases))
        }
        _ => false,
    };
    if !modules.iter().any(|m| m.ast.items.iter().any(callable)) {
        return (None, vec![]);
    }
    let mut diags = vec![];
    let out = modules
        .iter()
        .map(|m| {
            let mut ast = m.ast.clone();
            if ast.items.iter().any(callable) {
                let items = std::mem::take(&mut ast.items);
                ast.items = items
                    .into_iter()
                    .map(|item| match item.kind {
                        ast::ItemKind::Interface(d)
                            if !d.call_sigs.is_empty()
                                || d.extends.iter().any(|b| names_function(b, &fn_bases)) =>
                        {
                            ast::Item {
                                kind: ast::ItemKind::TypeAlias(interface_alias(
                                    d, item.span, &mut diags,
                                )),
                                ..item
                            }
                        }
                        _ => item,
                    })
                    .collect();
            }
            SourceModule {
                path: m.path.clone(),
                is_std: m.is_std,
                file: m.file,
                ast,
                imports: m.imports.clone(),
                jsx_runtime: m.jsx_runtime.clone(),
            }
        })
        .collect();
    (Some(out), diags)
}

/// The names of the type aliases of function types and of the interfaces with call signatures
/// (`type EffectFn = () => void`), and of the interfaces extending one, in any module: an
/// interface extending one (`interface Subscriber extends EffectFn { … }`) is a function with
/// properties too (#921).
fn function_names(modules: &[SourceModule]) -> rustc_hash::FxHashSet<String> {
    let mut names = rustc_hash::FxHashSet::default();
    let items = || {
        modules
            .iter()
            .filter(|m| !m.is_std)
            .flat_map(|m| &m.ast.items)
    };
    for item in items() {
        match &item.kind {
            ast::ItemKind::TypeAlias(a)
                if matches!(a.ty.kind, ast::TypeExprKind::Function { .. }) =>
            {
                names.insert(a.name.name.clone());
            }
            ast::ItemKind::Interface(d) if d.call_sigs.len() == 1 => {
                names.insert(d.name.name.clone());
            }
            _ => {}
        }
    }
    loop {
        let before = names.len();
        for item in items() {
            if let ast::ItemKind::Interface(d) = &item.kind {
                if d.extends.iter().any(|b| names_function(b, &names)) {
                    names.insert(d.name.name.clone());
                }
            }
        }
        if names.len() == before {
            return names;
        }
    }
}

/// Is `base` (an interface's `extends` entry) one of the `names` of `function_names`?
fn names_function(base: &ast::TypeExpr, names: &rustc_hash::FxHashSet<String>) -> bool {
    matches!(&base.kind, ast::TypeExprKind::Named { path, .. } if path.len() == 1 && names.contains(&path[0].name))
}

/// The type alias an interface with signatures declares (at `span`).
fn interface_alias(
    decl: ast::InterfaceDecl,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) -> ast::TypeAlias {
    let mut fields: Vec<ast::ObjectTypeField> = decl
        .fields
        .into_iter()
        .map(|f| ast::ObjectTypeField {
            name: f.name,
            // An object type's optional field has the `| undefined` an interface's leaves out.
            ty: match f.optional {
                true => or_undefined(f.ty),
                false => f.ty,
            },
            optional: f.optional,
            readonly: f.readonly,
            span: f.span,
            accessors: vec![],
        })
        .collect();
    for m in decl.methods {
        if m.is_getter || m.is_setter || m.body.is_some() {
            diags.push(Diagnostic::error(
                "an interface with call signatures can't have accessors or methods with bodies",
                m.sig.name.span,
            ));
            continue;
        }
        let field_span = m.sig.span;
        fields.push(ast::ObjectTypeField {
            name: m.sig.name.clone(),
            ty: fn_type(&m.sig, field_span),
            optional: false,
            readonly: false,
            span: field_span,
            accessors: vec![],
        });
    }
    let mut rest = decl.extends;
    if !fields.is_empty() {
        rest.push(ast::TypeExpr {
            kind: ast::TypeExprKind::Object(fields),
            span,
        });
    }
    ast::TypeAlias {
        name: decl.name,
        generics: decl.generics,
        ty: callable_type(&decl.call_sigs, rest, span),
    }
}

/// `T | undefined` for the written type of an optional field (the added `undefined` is
/// zero-width, as `velt_syntax` `or_undefined` adds it).
fn or_undefined(ty: ast::TypeExpr) -> ast::TypeExpr {
    let span = ty.span;
    let undefined = ast::TypeExpr {
        kind: ast::TypeExprKind::Undefined,
        span: Span::new(span.file, span.hi, span.hi),
    };
    match ty.kind {
        ast::TypeExprKind::Undefined => ty,
        ast::TypeExprKind::Union(mut ms) => {
            if !ms
                .iter()
                .any(|m| matches!(m.kind, ast::TypeExprKind::Undefined))
            {
                ms.push(undefined);
            }
            ast::TypeExpr {
                kind: ast::TypeExprKind::Union(ms),
                span,
            }
        }
        _ => ast::TypeExpr {
            kind: ast::TypeExprKind::Union(vec![ty, undefined]),
            span,
        },
    }
}

/// What the signatures of the user's modules use that is not supported: optional parameters
/// (not supported yet) and parameter initializers (TypeScript's TS2371).
pub(crate) fn check(modules: &[SourceModule]) -> Vec<Diagnostic> {
    struct Sigs(Vec<Diagnostic>);
    impl Sigs {
        fn sig(&mut self, s: &ast::CallSig) {
            for p in &s.sig.params {
                if p.optional {
                    self.0.push(
                        Diagnostic::error(
                            "optional parameters in call signatures are not supported yet",
                            p.span,
                        )
                        .with_note(format!(
                            "declare `{0}: T | undefined` and pass it at every call, or write a function type for each arity",
                            p.name.name
                        )),
                    );
                } else if p.default.is_some() && !p.rest {
                    self.0.push(Diagnostic::error(
                        "a parameter initializer is only allowed in a function or constructor implementation",
                        p.span,
                    ));
                }
            }
        }
    }
    impl<'a> Visit<'a> for Sigs {
        fn item(&mut self, item: &'a ast::Item) {
            if let ast::ItemKind::Interface(d) = &item.kind {
                d.call_sigs.iter().for_each(|s| self.sig(s));
            }
        }
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Callable { sigs, .. } = &t.kind {
                sigs.iter().for_each(|s| self.sig(s));
            }
        }
    }
    let mut v = Sigs(vec![]);
    for m in modules.iter().filter(|m| !m.is_std) {
        walk_module(&m.ast, &mut v);
    }
    v.0
}
