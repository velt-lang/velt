//! Written types (`ast::TypeExpr`) → interned `TyId`s.
//!
//! Names resolve in order: generic parameters in scope, primitives, builtin generic types
//! (`Array<T>`, `Result<T, E>`, `Promise<T>`, `shared<T>`), then module items (own, imported,
//! prelude); `ns.T` names export `T` of namespace import `ns`. Interfaces used as types are
//! `TyKind::Dyn`; `T | null` is `TyKind::Option`; other unions are compiler-generated enums (`crate::unions`); literal types are `TyKind::Literal`
//! (`crate::literals`) and object types `{ a: T }` anonymous object types (`crate::anon`).

use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};
use crate::type_defaults::DefaultsOf;

/// Generic parameter names in scope (`Param(i)` is `params[i]`) and the module for lookups.
#[derive(Clone, Default)]
pub(crate) struct TyEnv {
    pub module: usize,
    pub params: Vec<String>,
    /// While a generic alias is expanded: its type arguments at this use. Only the utility
    /// types read them (`Omit<P, "children">` in `type WithoutChildren<P>` needs `P`'s fields);
    /// everything else resolves `params` to type parameters and is substituted afterwards.
    pub args: Vec<TyId>,
}

impl TyEnv {
    pub fn new(module: usize, params: &[String]) -> Self {
        TyEnv {
            module,
            params: params.to_vec(),
            args: vec![],
        }
    }
}

impl Ctx<'_> {
    /// Resolve a written type. Reports and returns `Error` for unknown / invalid types.
    pub fn resolve_type(&mut self, t: &ast::TypeExpr, env: &TyEnv) -> TyId {
        match &t.kind {
            ast::TypeExprKind::Void => self.ty.unit,
            ast::TypeExprKind::Named { path, args } => self.resolve_named(t, path, args, env),
            ast::TypeExprKind::Array(e) => {
                let e = self.resolve_type(e, env);
                self.ty.array(e)
            }
            ast::TypeExprKind::Tuple(ts) => {
                let ts = ts.iter().map(|t| self.resolve_type(t, env)).collect();
                self.ty.intern(TyKind::Tuple(ts))
            }
            ast::TypeExprKind::Function {
                params,
                ret,
                throws,
            } => self.resolve_fn_type(params, ret, throws.as_deref(), env),
            ast::TypeExprKind::Union(parts) => self.resolve_union(t, parts, env),
            ast::TypeExprKind::Literal(l) => match self.lit_value_of(l, t.span) {
                Some(v) => self.lit_type(v),
                None => self.ty.error,
            },
            ast::TypeExprKind::Object(fields) => self.resolve_object(fields, env),
            ast::TypeExprKind::Null => {
                self.err("`null` alone is not a type; write `T | null`", t.span);
                self.ty.error
            }
        }
    }

    /// `(params) => ret [throws E]`. After a `Promise` result, `throws E` is the promise's
    /// rejection type (calling an async function never throws; awaiting its promise does).
    fn resolve_fn_type(
        &mut self,
        params: &[ast::TypeExpr],
        ret: &ast::TypeExpr,
        throws: Option<&ast::TypeExpr>,
        env: &TyEnv,
    ) -> TyId {
        let params = params.iter().map(|t| self.resolve_type(t, env)).collect();
        let mut ret = self.resolve_type(ret, env);
        let never = self.ty.never;
        let err = throws.map(|t| self.resolve_type(t, env));
        let err = self.canon_error(err).unwrap_or(never);
        let throws = match self.ty.kind(ret).clone() {
            TyKind::Promise(v, e) if err != never => {
                let e = self.join_errors(Some(e), Some(err)).unwrap_or(never);
                ret = self.ty.promise_rejecting(v, e);
                never
            }
            _ => err,
        };
        self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// `{ a: A; b: B }`: the anonymous object type of that shape.
    fn resolve_object(&mut self, fields: &[ast::ObjectTypeField], env: &TyEnv) -> TyId {
        let mut out: Vec<(String, TyId, bool)> = vec![];
        let mut spans = vec![];
        for f in fields {
            let t = self.resolve_type(&f.ty, env);
            if out.iter().any(|(n, ..)| *n == f.name.name) {
                self.err(
                    format!("duplicate field `{}` in object type", f.name.name),
                    f.name.span,
                );
                continue;
            }
            out.push((f.name.name.clone(), t, f.readonly));
            spans.push(f.name.span);
        }
        if out.iter().any(|(_, t, _)| *t == self.ty.error) {
            return self.ty.error;
        }
        let t = self.anon_type_with(&out, env.module);
        self.declare_anon_fields(t, &spans);
        t
    }

    /// `A | B | null` (canonicalized by `crate::unions`).
    fn resolve_union(&mut self, t: &ast::TypeExpr, parts: &[ast::TypeExpr], env: &TyEnv) -> TyId {
        let mut nullable = false;
        let mut members = vec![];
        for p in parts {
            match p.kind {
                ast::TypeExprKind::Null => nullable = true,
                _ => members.push(self.resolve_type(p, env)),
            }
        }
        self.union_of(&members, nullable, t.span)
    }

    fn resolve_named(
        &mut self,
        t: &ast::TypeExpr,
        path: &[ast::Ident],
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        if path.len() != 1 {
            let name: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            let name = name.join(".");
            let item = self.lookup_path_at(env.module, path, t.span);
            if let (Some(item), Some(last)) = (item, path.last()) {
                self.rec_item(last.span, Some(item));
                return self.item_type(item, &name, args, t, env);
            }
            self.err(format!("cannot find type `{name}` in this scope"), t.span);
            return self.ty.error;
        }
        let name = path[0].name.as_str();
        if let Some(i) = env.params.iter().rposition(|p| p == name) {
            if !args.is_empty() {
                self.err(
                    format!("type parameter `{name}` does not take type arguments"),
                    t.span,
                );
            }
            return self.ty.param(i as u32);
        }
        if let Some(p) = self.ty.primitive(name) {
            if !args.is_empty() {
                self.err(
                    format!("type `{name}` does not take type arguments"),
                    t.span,
                );
                return self.ty.error;
            }
            return p;
        }
        let item = self.lookup_item_at(env.module, name, t.span);
        self.rec_item(path[0].span, item);
        if item.is_none() {
            if let Some(b) = self.resolve_builtin(t, name, args, env) {
                return b;
            }
        }
        match item {
            Some(item) => self.item_type(item, name, args, t, env),
            None => {
                args.iter().for_each(|a| {
                    self.resolve_type(a, env);
                });
                self.err(format!("cannot find type `{name}` in this scope"), t.span);
                self.ty.error
            }
        }
    }

    /// The type an item named `name` denotes with type arguments `args`.
    pub(crate) fn item_type(
        &mut self,
        item: Item,
        name: &str,
        args: &[ast::TypeExpr],
        t: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        let args: Vec<TyId> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        match item {
            Item::Def(d) => self.def_type(d, name, args, t),
            Item::Alias(a) => self.expand_alias(a, args, t),
        }
    }

    /// `Array<T>`, `Promise<T>` / `Promise<T, E>`, `shared<T>` (unless shadowed by an item).
    fn resolve_builtin(
        &mut self,
        t: &ast::TypeExpr,
        name: &str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Option<TyId> {
        let arity = match name {
            "Array" | "shared" | "Shared" => 1,
            "Promise" if args.len() == 2 => 2,
            "Promise" => 1,
            "Result" => return Some(self.removed_result(t)),
            n if crate::utility_types::OPERATORS.contains(&n) => {
                return Some(self.resolve_utility(t, name, args, env))
            }
            _ => return None,
        };
        let args: Vec<TyId> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        if args.len() != arity {
            self.arity_error(name, arity, args.len(), t);
            return Some(self.ty.error);
        }
        let k = match name {
            "Array" => TyKind::Array(args[0]),
            "Promise" => {
                let e = args.get(1).copied().unwrap_or(self.ty.never);
                let e = self.canon_error(Some(e)).unwrap_or(self.ty.never);
                TyKind::Promise(args[0], e)
            }
            _ => TyKind::Shared(args[0]),
        };
        Some(self.ty.intern(k))
    }

    fn removed_result(&mut self, t: &ast::TypeExpr) -> TyId {
        self.error(
            velt_common::Diagnostic::error("`Result` was removed", t.span).with_note(
                "throw errors (a function's error types are part of its signature: `function f(): T throws E`), or return a union such as `User | NotFound`",
            ),
        );
        self.ty.error
    }

    fn arity_error(&mut self, name: &str, want: usize, got: usize, t: &ast::TypeExpr) {
        self.err(
            format!("type `{name}` takes {want} type argument(s) but {got} were supplied"),
            t.span,
        );
    }

    /// The type named by a type definition applied to `args`.
    fn def_type(
        &mut self,
        d: crate::hir::DefId,
        name: &str,
        args: Vec<TyId>,
        t: &ast::TypeExpr,
    ) -> TyId {
        let (arity, is_iface, decl) = match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => (
                a.generics.len(),
                false,
                a.decl.map(|x| (a.module, &x.generics[..])),
            ),
            DefInfo::Enum(e) => (e.generics.len(), false, None),
            DefInfo::Iface(i) => (
                i.generics.len(),
                true,
                i.decl.map(|x| (i.module, &x.generics[..])),
            ),
            _ => {
                self.err(format!("`{name}` is not a type"), t.span);
                return self.ty.error;
            }
        };
        let args = match decl {
            Some((module, gs)) => self.with_defaults(DefaultsOf::Def(d), module, gs, args),
            None => args,
        };
        if args.len() != arity {
            self.arity_error(name, arity, args.len(), t);
            return self.ty.error;
        }
        // `Record<K, V>`: a concrete `K` must be a key here; a type parameter is checked per
        // instantiation (`crate::record_keys`).
        if Some(d) == self.prelude_adt("Record") && !self.check_record_key(args[0], t.span, None) {
            return self.ty.error;
        }
        if let Some(&object) = self.field_only.get(&d) {
            // A field-only interface is an object type (`collect::field_only`).
            self.ty.intern(TyKind::Adt(object, args))
        } else if is_iface {
            self.ty.intern(TyKind::Dyn(d, args))
        } else {
            self.ty.intern(TyKind::Adt(d, args))
        }
    }

    fn expand_alias(&mut self, a: u32, args: Vec<TyId>, t: &ast::TypeExpr) -> TyId {
        let info = &self.aliases[a as usize];
        let (module, decl) = (info.module, info.decl);
        if info.expanding {
            self.err(
                format!("type alias `{}` refers to itself", decl.name.name),
                t.span,
            );
            return self.ty.error;
        }
        let args = self.with_defaults(DefaultsOf::Alias(a), module, &decl.generics, args);
        if args.len() != decl.generics.len() {
            self.arity_error(&decl.name.name, decl.generics.len(), args.len(), t);
            return self.ty.error;
        }
        let names: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
        self.aliases[a as usize].expanding = true;
        let mut env = TyEnv::new(module, &names);
        env.args = args.clone();
        let body = self.resolve_type(&decl.ty, &env);
        self.aliases[a as usize].expanding = false;
        if names.is_empty() && self.is_structural(body) {
            // `type Shape = { ... } | { ... }`: messages call the union `Shape`.
            self.alias_names
                .entry(body)
                .or_insert_with(|| decl.name.name.clone());
        }
        self.ty.subst(body, &args)
    }

    /// `args` of class or struct `d` completed with its parameters' defaults (`new Box<i64>(...)`
    /// for `class Box<T, E = never>`).
    pub(crate) fn adt_with_defaults(&mut self, d: crate::hir::DefId, args: Vec<TyId>) -> Vec<TyId> {
        let decl = match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => a.decl.map(|x| (a.module, x)),
            _ => None,
        };
        match decl {
            Some((module, x)) => self.with_defaults(DefaultsOf::Def(d), module, &x.generics, args),
            None => args,
        }
    }

    /// A type without a name of its own (a union or an anonymous object type).
    fn is_structural(&self, t: TyId) -> bool {
        self.union_def(t).is_some()
            || matches!(self.ty.kind(t), TyKind::Adt(d, _)
                if self.adt(*d).is_some_and(|a| a.kind == crate::hir::AdtKind::Anon))
    }
}
