//! Constants of a generic function type (#943): `const id: <T>(x: T) => T = (x) => x;`, also
//! through a type alias (`type Id = <T>(x: T) => T; const id: Id = …`, an alias imported from
//! another module, a generic alias `EmitFn<E>` at its arguments, or an interface with one
//! generic call signature, which `crate::call_sigs` made such an alias). TypeScript's value
//! stays generic: each call instantiates it. Velt checks such a constant as the generic function
//! it is, as it does a generic arrow constant (`super`), so each call instantiates it and
//! nothing is boxed:
//! - an arrow initializer is the function's body, its parameters typed by the arrow where it
//!   types them and by the function type elsewhere (parameters the arrow leaves out are hidden
//!   ones, as TypeScript lets a function take fewer parameters than its type);
//! - another initializer must be a property path (`const f: G = ns.f`); the function calls it,
//!   `function f<T>(a0: T): T { return (ns.f as (x: T) => T)(a0); }`, reading it at each call.
//!
//! A name (`const asA: A = id`) stays a constant holding that value (the same value), and a value
//! asserted to a generic function type (`const createToken = Symbol as unknown as <T>(d: string)
//! => Token<T>`) is one made once (`body::expr::fn_casts`).
//!
//! A generic function type anywhere else (a parameter, a field, a variable that is not such a
//! constant) is a value that stays generic, which Velt has no representation for yet: those
//! stay errors (`crate::fn_types::unsupported_fn_type`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;
use velt_syntax::ast::{self, TypeExprKind as K};

use crate::SourceModule;

/// The type aliases of every module and the names each module imports, to find the generic
/// function type a constant's annotation names. Empty (nothing collected) when no module
/// declares an alias of a generic function type: then only a type written out at the constant
/// is one.
#[derive(Default)]
pub(crate) struct GenericFnTypes<'m> {
    /// Per module: alias name → its declaration.
    aliases: Vec<FxHashMap<&'m str, &'m ast::TypeAlias>>,
    /// Per module: imported local name → (module index, exported name).
    imports: Vec<FxHashMap<&'m str, (usize, &'m str)>>,
    /// The names that may name an alias of a generic function type, in any module.
    names: FxHashSet<&'m str>,
}

/// A generic function type: its type parameters and its function type.
pub(crate) struct GenericFn {
    pub type_params: Vec<ast::GenericParam>,
    pub params: Vec<ast::TypeExpr>,
    pub ret: ast::TypeExpr,
    pub throws: Option<ast::TypeExpr>,
    pub rest: bool,
}

impl<'m> GenericFnTypes<'m> {
    /// The table for `modules` (empty when no module declares an alias of a function type, the
    /// common case).
    pub(crate) fn new(modules: &'m [SourceModule]) -> GenericFnTypes<'m> {
        let mut t = GenericFnTypes::default();
        // An alias of a function type of fixed types tells uses of a generic constant that
        // instantiate it (`const p: N = g`, `crate::generic_arrows::local`).
        let is_generic_alias = |i: &ast::Item| matches!(&i.kind, ast::ItemKind::TypeAlias(a) if matches!(a.ty.kind, K::GenericFunction { .. } | K::Function { .. }));
        if !modules
            .iter()
            .any(|m| m.ast.items.iter().any(is_generic_alias))
        {
            return t;
        }
        let index: FxHashMap<&str, usize> = modules
            .iter()
            .enumerate()
            .map(|(i, m)| (m.path.as_str(), i))
            .collect();
        for m in modules {
            let mut aliases = FxHashMap::default();
            let mut imports = FxHashMap::default();
            for item in &m.ast.items {
                match &item.kind {
                    ast::ItemKind::TypeAlias(a) => {
                        aliases.insert(a.name.name.as_str(), a);
                        // An alias of an alias may name one too.
                        if matches!(a.ty.kind, K::GenericFunction { .. } | K::Named { .. }) {
                            t.names.insert(a.name.name.as_str());
                        }
                    }
                    ast::ItemKind::Import(imp) if imp.namespace.is_none() && !imp.all => {
                        let Some(&target) = m
                            .imports
                            .iter()
                            .find(|(spec, _)| *spec == imp.from)
                            .and_then(|(_, path)| index.get(path.as_str()))
                        else {
                            continue;
                        };
                        for n in &imp.names {
                            let local = n.alias.as_ref().unwrap_or(&n.name);
                            imports.insert(local.name.as_str(), (target, n.name.name.as_str()));
                            if n.alias.is_some() {
                                t.names.insert(local.name.as_str());
                            }
                        }
                    }
                    _ => {}
                }
            }
            t.aliases.push(aliases);
            t.imports.push(imports);
        }
        t
    }

    /// May `v` be a constant of a generic function type? (A quick test before
    /// [`typed_function`].)
    pub(crate) fn may_declare(&self, v: &ast::VarDecl) -> bool {
        if v.kind != ast::VarKind::Const {
            return false;
        }
        let named = |t: &ast::TypeExpr| match &t.kind {
            K::GenericFunction { .. } => true,
            K::Named { path, .. } => path.len() == 1 && self.names.contains(path[0].name.as_str()),
            _ => false,
        };
        match (&v.ty, v.init.as_ref().map(|e| &e.kind)) {
            (Some(t), Some(_)) => named(t),
            _ => false,
        }
    }

    /// The generic function type `t` is in module `module` (written out, or an alias naming
    /// one at its arguments), if it is one.
    pub(crate) fn generic_fn(&self, module: usize, t: &ast::TypeExpr) -> Option<GenericFn> {
        self.lookup(module, t, 0)
    }

    fn lookup(&self, module: usize, t: &ast::TypeExpr, depth: u32) -> Option<GenericFn> {
        match &t.kind {
            K::GenericFunction { type_params, ty } => match &ty.kind {
                K::Function {
                    params,
                    ret,
                    throws,
                    rest,
                } => Some(GenericFn {
                    type_params: type_params.clone(),
                    params: params.clone(),
                    ret: (**ret).clone(),
                    throws: throws.as_deref().cloned(),
                    rest: *rest,
                }),
                _ => None,
            },
            K::Named { path, args } if path.len() == 1 && depth < 16 => {
                let (m, alias) = self.alias(module, &path[0].name, 0)?;
                if args.len() > alias.generics.len() {
                    return None;
                }
                // Arguments left out take their defaults (which may name earlier parameters).
                let mut map: Vec<(String, ast::TypeExpr)> = vec![];
                for (i, g) in alias.generics.iter().enumerate() {
                    let arg = match args.get(i) {
                        Some(a) => a.clone(),
                        None => subst(g.default.as_ref()?, &map),
                    };
                    map.push((g.name.name.clone(), arg));
                }
                // The function is declared where the constant is: the alias's own names must
                // mean the same there.
                if m != module && !self.readable_in(&alias.ty, &alias.generics, m, module) {
                    return None;
                }
                let body = subst(&alias.ty, &map);
                self.lookup(module, &body, depth + 1)
            }
            _ => None,
        }
    }

    /// Does every name in `t` (the body of an alias of module `from` with parameters `params`)
    /// mean the same in module `to`: a built-in type, a parameter, or a name `to` imports from
    /// `from`? (Type parameters of inner generic function types count as parameters too.)
    fn readable_in(
        &self,
        t: &ast::TypeExpr,
        params: &[ast::GenericParam],
        from: usize,
        to: usize,
    ) -> bool {
        struct Names<'n> {
            ok: bool,
            local: Vec<String>,
            imports: Option<&'n FxHashMap<&'n str, (usize, &'n str)>>,
            from: usize,
        }
        impl<'a> velt_syntax::visit::Visit<'a> for Names<'_> {
            fn ty(&mut self, t: &'a ast::TypeExpr) {
                if let K::GenericFunction { type_params, .. } = &t.kind {
                    self.local
                        .extend(type_params.iter().map(|g| g.name.name.clone()));
                }
                let (K::Named { path, .. } | K::Query(path)) = &t.kind else {
                    return;
                };
                let Some(first) = path.first() else { return };
                let n = first.name.as_str();
                let builtin = matches!(
                    n,
                    "string"
                        | "number"
                        | "boolean"
                        | "bigint"
                        | "symbol"
                        | "unknown"
                        | "any"
                        | "object"
                        | "never"
                        | "Array"
                        | "Promise"
                        | "Map"
                        | "Set"
                        | "Record"
                );
                let imported = self
                    .imports
                    .and_then(|i| i.get(n))
                    .is_some_and(|&(m, e)| m == self.from && e == n);
                self.ok &= builtin || imported || self.local.iter().any(|l| l == n);
            }
        }
        let mut v = Names {
            ok: true,
            local: params.iter().map(|g| g.name.name.clone()).collect(),
            imports: self.imports.get(to),
            from,
        };
        velt_syntax::visit::walk_type_expr(t, &mut v);
        v.ok
    }

    /// The alias named `name` in module `module`, declared there or imported, with the module
    /// declaring it.
    /// Is `t` (in module `module`) a function type of fixed types: written out, or an alias of
    /// one without type parameters (also imported, or an alias of such an alias)?
    pub(crate) fn fixed_fn(&self, module: usize, t: &ast::TypeExpr) -> bool {
        self.fixed_at(module, t, 0)
    }

    fn fixed_at(&self, module: usize, t: &ast::TypeExpr, depth: u32) -> bool {
        match &t.kind {
            K::Function { .. } => true,
            K::Named { path, args } if path.len() == 1 && args.is_empty() && depth < 16 => {
                match self.alias(module, &path[0].name, 0) {
                    Some((m, a)) if a.generics.is_empty() => self.fixed_at(m, &a.ty, depth + 1),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn alias(&self, module: usize, name: &str, depth: u32) -> Option<(usize, &'m ast::TypeAlias)> {
        if let Some(a) = self.aliases.get(module)?.get(name) {
            return Some((module, *a));
        }
        let &(m, exported) = self.imports.get(module)?.get(name)?;
        if depth > 8 {
            return None;
        }
        self.alias(m, exported, depth + 1)
    }
}

/// The function a constant `v` of a generic function type declares in module `module` (module
/// docs), or `None` when `v` is not one or has a form that stays an error.
pub(crate) fn typed_function(
    v: &ast::VarDecl,
    module: usize,
    table: &GenericFnTypes<'_>,
) -> Option<ast::FnDecl> {
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        return None;
    };
    if v.kind != ast::VarKind::Const {
        return None;
    }
    let init = v.init.as_ref()?;
    match (&v.ty, &init.kind) {
        (Some(t), ast::ExprKind::Arrow { .. }) => {
            let g = table.generic_fn(module, t)?;
            from_arrow(name, g, init, v.span)
        }
        // A name (`const asA: A = id`) stays a constant holding that value, so it is the
        // same value (`asA === id`, #943); a use of it as a function of fixed types is that
        // function (`body::expr::generic_conversions` `named_fn_of`).
        (Some(_), ast::ExprKind::Ident(_)) => None,
        (Some(t), _) => {
            let g = table.generic_fn(module, t)?;
            calling(name, g, init, v.span)
        }
        _ => None,
    }
}

/// `const f: <T>(p…) => R = (q…) => body;` as `function f<T>(q…): R { return body; }`.
fn from_arrow(
    name: &ast::Ident,
    g: GenericFn,
    init: &ast::Expr,
    span: Span,
) -> Option<ast::FnDecl> {
    let ast::ExprKind::Arrow {
        type_params,
        params,
        ret,
        throws,
        body,
        is_async,
    } = &init.kind
    else {
        return None;
    };
    if !type_params.is_empty() || params.len() > g.params.len() {
        return None;
    }
    let last = g.params.len().saturating_sub(1);
    let mut out = vec![];
    for (i, want) in g.params.iter().enumerate() {
        let rest = g.rest && i == last;
        let Some(p) = params.get(i) else {
            out.push(hidden_param(i, want, rest));
            continue;
        };
        if p.rest != rest {
            return None;
        }
        let ty = p.ty.clone().unwrap_or_else(|| want.clone());
        let mut default = p.default.clone();
        let mut optional = p.optional;
        if default.is_none() && is_optional(want) && !rest {
            default = Some(undefined_at(ty.span));
            optional = true;
        }
        if rest {
            default = Some(empty_array_at(ty.span));
        }
        out.push(ast::Param {
            span: p.name.span.to(ty.span),
            name: p.name.clone(),
            pattern: p.pattern.clone(),
            ty,
            default,
            optional,
            rest,
        });
    }
    let body = match body {
        ast::ArrowBody::Block(b) => b.clone(),
        ast::ArrowBody::Expr(e) => ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Return(Some((**e).clone())),
                span: e.span,
            }],
            span: e.span,
        },
    };
    let ret = ret.clone().unwrap_or(g.ret);
    let throws = throws.clone().or(g.throws);
    Some(decl(
        name,
        g.type_params,
        out,
        ret,
        throws,
        *is_async,
        body,
        span,
    ))
}

/// `const f: <T>(p…) => R = x;` as `function f<T>(a…): R { return (x as (p…) => R)(a…); }`:
/// the value called at the instantiated function type.
fn calling(name: &ast::Ident, g: GenericFn, init: &ast::Expr, span: Span) -> Option<ast::FnDecl> {
    let value = init.clone();
    if !is_path(strip(&value)) {
        return None;
    }
    let at = init.span;
    let fn_ty = ast::TypeExpr {
        kind: K::Function {
            params: g.params.clone(),
            ret: Box::new(g.ret.clone()),
            throws: g.throws.clone().map(Box::new),
            rest: g.rest,
        },
        span: at,
    };
    let callee = expr(
        ast::ExprKind::Paren(Box::new(expr(
            ast::ExprKind::Cast {
                expr: Box::new(value),
                ty: fn_ty,
            },
            at,
        ))),
        at,
    );
    let last = g.params.len().saturating_sub(1);
    let params: Vec<ast::Param> = g
        .params
        .iter()
        .enumerate()
        .map(|(i, t)| hidden_param(i, t, g.rest && i == last))
        .collect();
    let args = params
        .iter()
        .map(|p| {
            let read = expr(ast::ExprKind::Ident(p.name.clone()), at);
            match p.rest {
                true => expr(ast::ExprKind::Spread(Box::new(read)), at),
                false => read,
            }
        })
        .collect();
    let call = expr(
        ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        },
        at,
    );
    let stmt = match g.ret.kind {
        K::Void => ast::StmtKind::Expr(call),
        _ => ast::StmtKind::Return(Some(call)),
    };
    let body = ast::Block {
        stmts: vec![ast::Stmt {
            kind: stmt,
            span: at,
        }],
        span: at,
    };
    Some(decl(
        name,
        g.type_params,
        params,
        g.ret,
        g.throws,
        false,
        body,
        span,
    ))
}

#[allow(clippy::too_many_arguments)]
fn decl(
    name: &ast::Ident,
    generics: Vec<ast::GenericParam>,
    params: Vec<ast::Param>,
    ret: ast::TypeExpr,
    throws: Option<ast::TypeExpr>,
    is_async: bool,
    body: ast::Block,
    span: Span,
) -> ast::FnDecl {
    ast::FnDecl {
        sig: ast::FnSig {
            name: name.clone(),
            generics,
            params,
            ret: Some(ret),
            throws,
            is_async,
            is_generator: false,
            this_param: None,
            span,
        },
        body,
        overloads: vec![],
    }
}

/// Parameter `i` of a function type, under a name no source can spell.
fn hidden_param(i: usize, ty: &ast::TypeExpr, rest: bool) -> ast::Param {
    let name = ast::Ident {
        name: format!("#arg{i}"),
        span: Span::new(ty.span.file, ty.span.lo, ty.span.lo),
    };
    let optional = !rest && is_optional(ty);
    let default = match (rest, optional) {
        (true, _) => Some(empty_array_at(ty.span)),
        (false, true) => Some(undefined_at(ty.span)),
        (false, false) => None,
    };
    ast::Param {
        span: ty.span,
        name,
        pattern: None,
        ty: ty.clone(),
        default,
        optional,
        rest,
    }
}

/// Is `t` the type of an optional parameter (`x?: T`, whose `| undefined` the parser added at
/// zero width)?
fn is_optional(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        K::Union(ms) => ms
            .last()
            .is_some_and(|m| matches!(m.kind, K::Undefined) && m.span.lo == m.span.hi),
        _ => false,
    }
}

fn expr(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn undefined_at(span: Span) -> ast::Expr {
    expr(
        ast::ExprKind::Lit(ast::Lit::Undefined),
        Span::new(span.file, span.hi, span.hi),
    )
}

fn empty_array_at(span: Span) -> ast::Expr {
    expr(
        ast::ExprKind::Array(vec![]),
        Span::new(span.file, span.hi, span.hi),
    )
}

/// `e` without parentheses and assertions.
fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) | ast::ExprKind::Cast { expr: x, .. } => strip(x),
        _ => e,
    }
}

/// Is `e` a name or a property path of one (`Symbol`, `ns.f`)? Reading it again at each call
/// reads what the constant would have held.
fn is_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_path(object),
        _ => false,
    }
}

/// `t` with each name in `map` (a type parameter of an alias) replaced by its type.
fn subst(t: &ast::TypeExpr, map: &[(String, ast::TypeExpr)]) -> ast::TypeExpr {
    let mut t = t.clone();
    if !map.is_empty() {
        Subst { map }.ty(&mut t);
    }
    t
}

struct Subst<'m> {
    map: &'m [(String, ast::TypeExpr)],
}

impl Subst<'_> {
    /// The substitution without the names `names` declares (they shadow the alias's).
    fn without(&self, names: &[&str]) -> Vec<(String, ast::TypeExpr)> {
        self.map
            .iter()
            .filter(|(n, _)| !names.contains(&n.as_str()))
            .cloned()
            .collect()
    }

    fn ty(&self, t: &mut ast::TypeExpr) {
        match &mut t.kind {
            K::Named { path, args } => {
                if path.len() == 1 && args.is_empty() {
                    if let Some((_, to)) = self.map.iter().find(|(n, _)| *n == path[0].name) {
                        *t = to.clone();
                        return;
                    }
                }
                args.iter_mut().for_each(|a| self.ty(a));
            }
            K::Array(e) | K::KeyOf(e) | K::Construct(e) | K::TupleRest(e) | K::TupleOptional(e) => {
                self.ty(e)
            }
            K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => {
                ts.iter_mut().for_each(|t| self.ty(t))
            }
            K::Function {
                params,
                ret,
                throws,
                ..
            } => {
                params.iter_mut().for_each(|t| self.ty(t));
                self.ty(ret);
                if let Some(e) = throws {
                    self.ty(e);
                }
            }
            K::GenericFunction { type_params, ty } => {
                let names: Vec<&str> = type_params.iter().map(|g| g.name.name.as_str()).collect();
                let map = self.without(&names);
                let inner = Subst { map: &map };
                for g in type_params.iter_mut() {
                    g.bounds.iter_mut().for_each(|b| inner.ty(b));
                    if let Some(d) = &mut g.default {
                        inner.ty(d);
                    }
                }
                inner.ty(ty);
            }
            K::Indexed { object, key } => {
                self.ty(object);
                self.ty(key);
            }
            K::Object(fields) => fields.iter_mut().for_each(|f| self.ty(&mut f.ty)),
            K::IndexSig {
                fields, key, value, ..
            } => {
                fields.iter_mut().for_each(|f| self.ty(&mut f.ty));
                self.ty(&mut key.ty);
                self.ty(value);
            }
            K::Predicate { ty: Some(t), .. } => self.ty(t),
            K::Callable { sigs, members } => {
                for s in sigs.iter_mut() {
                    let names: Vec<&str> = s
                        .sig
                        .generics
                        .iter()
                        .map(|g| g.name.name.as_str())
                        .collect();
                    let map = self.without(&names);
                    let inner = Subst { map: &map };
                    for p in s.sig.params.iter_mut() {
                        inner.ty(&mut p.ty);
                    }
                    if let Some(r) = &mut s.sig.ret {
                        inner.ty(r);
                    }
                }
                self.ty(members);
            }
            K::Conditional(c) => {
                self.ty(&mut c.check);
                self.ty(&mut c.extends);
                self.ty(&mut c.then);
                self.ty(&mut c.else_);
            }
            K::Mapped(m) => {
                self.ty(&mut m.source);
                let map = self.without(&[m.param.name.as_str()]);
                let inner = Subst { map: &map };
                if let Some(n) = &mut m.name {
                    inner.ty(n);
                }
                inner.ty(&mut m.value);
            }
            K::TemplateLiteral { types, .. } => types.iter_mut().for_each(|t| self.ty(t)),
            K::Predicate { ty: None, .. }
            | K::Infer(_)
            | K::Literal(_)
            | K::Query(_)
            | K::Null
            | K::Undefined
            | K::Void => {}
        }
    }
}
