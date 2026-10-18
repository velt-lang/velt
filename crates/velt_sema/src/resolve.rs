//! Written types (`ast::TypeExpr`) → interned `TyId`s.
//!
//! Names resolve in order: generic parameters in scope, primitives, builtin generic types
//! (`Array<T>`, `Result<T, E>`, `Promise<T>`, `shared<T>`), then module items (own, imported,
//! prelude); `ns.T` names export `T` of namespace import `ns`. Interfaces used as types are
//! `TyKind::Dyn`; `T | null` is `TyKind::Option`; other unions are compiler-generated enums (`crate::unions`); literal types are `TyKind::Literal`
//! (`crate::literals`) and object types `{ a: T }` anonymous object types (`crate::anon`).

use velt_common::Span;
use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{Nothing, TyId, TyKind};
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
    /// Names a type binds, innermost last: a mapped type's key, what `infer X` found, the
    /// member a distributive conditional type is at (`crate::type_level`).
    pub locals: Vec<(String, TyId)>,
    /// `infer X` names that found nothing (TypeScript's `unknown`).
    pub unknowns: Vec<String>,
    /// Inside a conditional or mapped type: `T[K]` of an optional field is `F | undefined`, as
    /// TypeScript reads it.
    pub ts_optional: bool,
    /// In a class's instance method (its signature and body): the class type, which the
    /// polymorphic `this` type resolves to (`m(): this`).
    pub this_ty: Option<TyId>,
}

impl TyEnv {
    pub fn new(module: usize, params: &[String]) -> Self {
        TyEnv {
            module,
            params: params.to_vec(),
            args: vec![],
            locals: vec![],
            unknowns: vec![],
            ts_optional: false,
            this_ty: None,
        }
    }
}

impl Ctx<'_> {
    /// Resolve a written type. Reports and returns `Error` for unknown / invalid types.
    pub fn resolve_type(&mut self, t: &ast::TypeExpr, env: &TyEnv) -> TyId {
        match &t.kind {
            ast::TypeExprKind::Void => self.ty.unit,
            // `x is T` returns a `boolean`; `asserts x is T` returns nothing (it throws instead
            // of returning `false`). The narrowing is `body::predicates`.
            ast::TypeExprKind::Predicate { ty, asserts, .. } => {
                if let Some(t) = ty {
                    self.resolve_type(t, env);
                }
                match asserts {
                    true => self.ty.unit,
                    false => self.ty.bool_,
                }
            }
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
                rest: false,
            } => self.resolve_fn_type(params, ret, throws.as_deref(), env),
            ast::TypeExprKind::TupleRest(_) => {
                self.error(
                    velt_common::Diagnostic::error(
                        "variadic tuple types (`[A, ...T]`) are not supported yet (#943)",
                        t.span,
                    )
                    .with_note("a tuple has a fixed number of elements; write them out (`[A, B, C]`), or use an array (`A[]`)"),
                );
                self.ty.error
            }
            ast::TypeExprKind::TupleOptional(_) => {
                self.error(
                    velt_common::Diagnostic::error(
                        "optional tuple elements (`[A, B?]`) are not supported yet (#938)",
                        t.span,
                    )
                    .with_note("a tuple has a fixed number of elements; make the element `B | null` and pass `null`, or declare one tuple type per length"),
                );
                self.ty.error
            }
            ast::TypeExprKind::Function { rest: true, .. }
            | ast::TypeExprKind::GenericFunction { .. }
            | ast::TypeExprKind::Construct(_) => {
                let (msg, note) = unsupported_fn_type(&t.kind);
                self.error(velt_common::Diagnostic::error(msg, t.span).with_note(note));
                self.ty.error
            }
            ast::TypeExprKind::Union(parts) => self.resolve_union(t, parts, env),
            ast::TypeExprKind::Intersection(parts) => self.resolve_intersection(t, parts, env),
            ast::TypeExprKind::Indexed { object, key } => self.resolve_indexed(object, key, env),
            ast::TypeExprKind::Literal(l) => match self.lit_value_of(l, t.span) {
                Some(v) => self.lit_type(v),
                None => self.ty.error,
            },
            ast::TypeExprKind::Object(fields) => self.resolve_object(fields, env),
            ast::TypeExprKind::IndexSig {
                fields, key, value, ..
            } => self.resolve_index_sig(fields, &key.ty, value, env),
            ast::TypeExprKind::Callable { sigs, members } => {
                let lowered = crate::call_sigs::lower(sigs, members, t.span);
                self.resolve_type(&lowered, env)
            }
            ast::TypeExprKind::Query(path) => self.resolve_query(t, path, env),
            ast::TypeExprKind::KeyOf(inner) => self.resolve_keyof(t, inner, env),
            ast::TypeExprKind::Conditional(c) => self.resolve_conditional(t, c, env),
            ast::TypeExprKind::Mapped(m) => self.resolve_mapped(t, m, env),
            ast::TypeExprKind::TemplateLiteral { quasis, types } => {
                self.resolve_template(t, quasis, types, env)
            }
            ast::TypeExprKind::Infer(n) => {
                match env.locals.iter().rev().find(|(l, _)| *l == n.name) {
                    Some((_, b)) => *b,
                    None => {
                        self.err(
                            "`infer` declarations are only permitted in the `extends` clause of a conditional type",
                            t.span,
                        );
                        self.ty.error
                    }
                }
            }
            ast::TypeExprKind::Null => self.ty.null_alone(),
            ast::TypeExprKind::Undefined => {
                self.writes_undefined = true;
                self.ty.undefined()
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
        let err = throws.map(|t| {
            let e = self.resolve_type(t, env);
            self.no_void_error(e, t.span);
            e
        });
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
        let mut out: Vec<ShapeField> = vec![];
        let mut spans = vec![];
        for f in fields {
            // The declared type: without the `| null` the parser adds for `name?: T`
            // (`velt_syntax` `or_null`), which the flag stands for (P2, deferred-types.md).
            let written = if f.optional {
                written_type(&f.ty)
            } else {
                f.ty.clone()
            };
            let t = self.resolve_type(&written, env);
            let name = self.member_key(env.module, &f.name);
            if out.iter().any(|o| o.name == name) {
                self.err(
                    format!("duplicate field `{name}` in object type"),
                    f.name.span,
                );
                continue;
            }
            out.push(ShapeField {
                name,
                ty: t,
                readonly: f.readonly,
                optional: f.optional,
            });
            spans.push(f.name.span);
        }
        if out.iter().any(|f| f.ty == self.ty.error) {
            return self.ty.error;
        }
        let t = self.anon_type_with(&out, env.module);
        self.declare_anon_fields(t, &spans);
        let str_ = self.ty.str_;
        for f in &out {
            self.record_key_kind(t, &f.name, str_);
        }
        t
    }

    /// `A | B | null` (canonicalized by `crate::unions`).
    fn resolve_union(&mut self, t: &ast::TypeExpr, parts: &[ast::TypeExpr], env: &TyEnv) -> TyId {
        let mut nothing: Option<Nothing> = None;
        let mut members = vec![];
        for p in parts {
            let n = match p.kind {
                ast::TypeExprKind::Null => Nothing::Null,
                // `T | void` is `T | undefined` (`crate::unions`).
                ast::TypeExprKind::Undefined | ast::TypeExprKind::Void => {
                    self.writes_undefined |= !self.scopes[env.module].is_std;
                    Nothing::Undefined
                }
                _ => {
                    members.push(self.resolve_type(p, env));
                    continue;
                }
            };
            nothing = Some(nothing.map_or(n, |m| m.join(n)));
        }
        self.union_with(&members, nothing, t.span)
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
            // A member of a namespace import of a module the parser recovered may be a
            // declaration it dropped (`Ctx::recovered`).
            let recovered = self.scopes[env.module]
                .namespaces
                .get(&path[0].name)
                .is_some_and(|&t| {
                    let member = path.last().map_or("", |i| i.name.as_str());
                    crate::collect::exports::maybe_dropped(self, t, member)
                });
            if recovered {
                return self.ty.error;
            }
            // A namespace declaration's member that is missing or not exported (TS2694).
            let (ns, member) = name.rsplit_once('.').unwrap_or_default();
            let prefix = format!("{ns}.");
            let is_namespace = !self.scopes[env.module].namespaces.contains_key(ns)
                && self.scopes[env.module]
                    .items
                    .keys()
                    .any(|k| k.starts_with(&prefix));
            match is_namespace {
                true => self.err(
                    format!("namespace `{ns}` has no exported member `{member}`"),
                    path.last().map_or(t.span, |i| i.span),
                ),
                false => self.err(format!("cannot find type `{name}` in this scope"), t.span),
            }
            return self.ty.error;
        }
        let name = path[0].name.as_str();
        if let Some((_, b)) = env.locals.iter().rev().find(|(l, _)| l == name) {
            if !args.is_empty() {
                self.err(
                    format!("type parameter `{name}` does not take type arguments"),
                    t.span,
                );
            }
            return *b;
        }
        if env.unknowns.iter().any(|u| u == name) {
            self.err(
                format!(
                    "`{name}` is inferred as `unknown` here, which is not supported yet (#934)"
                ),
                t.span,
            );
            return self.ty.error;
        }
        if name == "this" {
            return self.resolve_this_type(t, args, env);
        }
        if let Some(i) = env.params.iter().rposition(|p| p == name) {
            if !args.is_empty() {
                self.err(
                    format!("type parameter `{name}` does not take type arguments"),
                    t.span,
                );
            }
            return self.ty.param(i as u32);
        }
        // The SIMD type names (`u8x16`) are built in only for std: other modules import them
        // from `velt:simd` (#1055), whose `export type u8x16 = u8x16` names the built-in type.
        let simd_outside_std =
            crate::hir::SimdTy::named(name).is_some() && !self.scopes[env.module].is_std;
        // Velt's own type names (`u8`, `f32`, `bool`, …) are like library globals: a type the
        // program declares or imports shadows them, as TypeScript code may declare one (#1074).
        // A value of that name does not (a function `f64`). TypeScript's own (`number`,
        // `string`, …) cannot be declared.
        let shadowed = crate::types::velt_only_type_name(name)
            && self
                .lookup_item_at(env.module, name, t.span)
                .is_some_and(|item| self.is_type_item(item));
        if let Some(p) = self
            .ty
            .primitive(name)
            .filter(|_| !simd_outside_std && !shadowed)
        {
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
                self.not_found(
                    format!("cannot find type `{name}` in this scope"),
                    name,
                    t.span,
                );
                self.ty.error
            }
        }
    }

    /// The polymorphic `this` type: the class of the instance method it is written in. A call
    /// of a method returning `this` gives the receiver's own (sub)class type
    /// (`body::expr::this_type`).
    fn resolve_this_type(
        &mut self,
        t: &ast::TypeExpr,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        if !args.is_empty() {
            self.err("type `this` does not take type arguments", t.span);
            return self.ty.error;
        }
        match env.this_ty {
            Some(ty) => ty,
            None => {
                self.error(
                    velt_common::Diagnostic::error(
                        "a 'this' type is available only in a non-static member of a class or interface",
                        t.span,
                    )
                    .with_note("TypeScript's TS2526; in an interface, Velt doesn't support it yet: write the interface's name"),
                );
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
        if args.len() == 2 && self.is_iterator_result_alias(item) {
            return match self.iterator_result_args(args, env) {
                Some(args) => self.item_type_args(item, name, args, t),
                None => self.ty.error,
            };
        }
        let protocol = match item {
            Item::Def(d) if args.len() >= 2 => self.protocol_type(d),
            _ => None,
        };
        let args: Vec<TyId> = match protocol {
            // TypeScript's `Generator<T, TReturn, TNext>` spellings (`crate::ts_protocol`).
            Some(p) => match self.protocol_args(p, args, env) {
                Some(args) => args,
                None => return self.ty.error,
            },
            None => args.iter().map(|a| self.resolve_type(a, env)).collect(),
        };
        // Inside an alias expanded at known arguments, an alias used with the parameters gets
        // the arguments themselves, so a conditional or mapped type in it can be evaluated
        // (`type A<T> = B<T>`; `crate::type_level`).
        let args = match item {
            Item::Alias(_) if !env.args.is_empty() => {
                let known: Vec<TyId> = args.iter().map(|a| self.subst(*a, &env.args)).collect();
                match known.iter().any(|a| self.has_type_params(*a)) {
                    true => args,
                    false => known,
                }
            }
            _ => args,
        };
        self.item_type_args(item, name, args, t)
    }

    /// The type an item named `name` denotes with the resolved type arguments `args`.
    fn item_type_args(
        &mut self,
        item: Item,
        name: &str,
        args: Vec<TyId>,
        t: &ast::TypeExpr,
    ) -> TyId {
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
            // `ReadonlyArray<T>` (and `readonly T[]`, parsed as it) is `T[]` for now: TypeScript's
            // checks that it is not written to are not made yet.
            "Array" | "ReadonlyArray" | "shared" | "Shared" => 1,
            "Promise" if args.len() == 2 => 2,
            "Promise" => 1,
            "Result" => return Some(self.removed_result(t)),
            "ReturnType" | "Parameters" => {
                return Some(self.resolve_fn_operator(t, name, args, env))
            }
            n if crate::utility_types::OPERATORS.contains(&n) => {
                return Some(self.resolve_utility(t, name, args, env))
            }
            "NonNullable" | "Exclude" | "Extract" | "Uppercase" | "Lowercase" | "Capitalize"
            | "Uncapitalize" => return self.resolve_type_level_builtin(t, name, args, env),
            _ => return None,
        };
        let written = args;
        let args: Vec<TyId> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        if args.len() != arity {
            self.arity_error(name, arity, args.len(), t);
            return Some(self.ty.error);
        }
        let k = match name {
            "Array" | "ReadonlyArray" => TyKind::Array(args[0]),
            "Promise" => {
                let e = args.get(1).copied().unwrap_or(self.ty.never);
                if let Some(w) = written.get(1) {
                    self.no_void_error(e, w.span);
                }
                let e = self.canon_error(Some(e)).unwrap_or(self.ty.never);
                TyKind::Promise(args[0], e)
            }
            _ => TyKind::Shared(args[0]),
        };
        Some(self.ty.intern(k))
    }

    /// `void` written as an error type (`throws void`, `Promise<T, void>`): it throws nothing.
    pub(crate) fn no_void_error(&mut self, e: TyId, span: velt_common::Span) {
        if e == self.ty.unit {
            self.error(
                velt_common::Diagnostic::error("`void` is not an error type", span).with_note(
                    "leave the error type out: `throws` and `Promise<T>` without one mean nothing is thrown",
                ),
            );
        }
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

    /// Does `item` name a type (an alias, class, struct, enum or interface)?
    fn is_type_item(&self, item: Item) -> bool {
        match item {
            Item::Alias(_) => true,
            Item::Def(d) => matches!(
                self.info[d.0 as usize],
                DefInfo::Adt(_) | DefInfo::Enum(_) | DefInfo::Iface(_)
            ),
        }
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
            if let Some(own) = self.recursive_union_ref(a) {
                return own;
            }
            let d = velt_common::Diagnostic::error(
                format!("type alias `{}` refers to itself", decl.name.name),
                t.span,
            )
            .with_note("an alias may refer to itself when it is an object type or an intersection of object types written out (`{ kids: T[] } & { v: number }`) with no field named twice, or a union that names itself behind an array, an index signature, a `Map` or a function type (`type Json = string | Json[]`); give any other recursive type a nominal member, such as an interface or a class");
            self.error(d);
            return self.ty.error;
        }
        let args = self.with_defaults(DefaultsOf::Alias(a), module, &decl.generics, args);
        if args.len() != decl.generics.len() {
            self.arity_error(&decl.name.name, decl.generics.len(), args.len(), t);
            return self.ty.error;
        }
        if let Some(own) = self.recursive_union(a, t.span) {
            return own;
        }
        self.alias_body(a, args, t.span)
    }

    /// The body of alias `a` (with as many `args` as it has parameters) at `args`, used at
    /// `span`. A conditional, mapped, indexed access or template literal type in it that needs
    /// an argument that is still a type parameter makes it the deferred `TyKind::Op` (#934).
    pub(crate) fn alias_body(&mut self, a: u32, args: Vec<TyId>, span: Span) -> TyId {
        let info = &self.aliases[a as usize];
        let decl = info.decl;
        let module = info.module;
        let names: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
        self.aliases[a as usize].used = true;
        self.aliases[a as usize].expanding = true;
        let mut env = TyEnv::new(module, &names);
        env.args = args.clone();
        let outermost = self.type_level.alias_site.is_none();
        if outermost {
            self.type_level.alias_site = Some(span);
        }
        let defer = !self.checking_unused_aliases && args.iter().any(|x| self.has_type_params(*x));
        if defer {
            self.type_level.deferring += 1;
        }
        let outer_stuck = std::mem::replace(&mut self.type_level.stuck, false);
        let reported = self.diags.len();
        let body = self.resolve_type(&decl.ty, &env);
        let stuck = std::mem::replace(&mut self.type_level.stuck, outer_stuck);
        if defer {
            self.type_level.deferring -= 1;
        }
        if outermost {
            self.type_level.alias_site = None;
        }
        self.aliases[a as usize].expanding = false;
        if stuck {
            // Evaluated again, and reported, once the arguments are known.
            self.diags.truncate(reported);
            return self.deferred_alias(a, args, span);
        }
        if names.is_empty() && self.is_structural(body) {
            // `type Shape = { ... } | { ... }`: messages call the union `Shape`.
            self.alias_names
                .entry(body)
                .or_insert_with(|| decl.name.name.clone());
        }
        if self.mixes_nothings(body, &args) {
            self.err(
                "this alias keeps a `P | null` of a type argument that allows `undefined`, whose `undefined` it could not tell from its own `null` yet (#930)",
                span,
            );
            return self.ty.error;
        }
        self.subst(body, &args)
    }

    /// Does `body` (an alias's type, in terms of its parameters) hold a `P | null` or
    /// `P | undefined` of a parameter whose argument in `args` allows the other nothing? The
    /// substitution would make an option of an option, which keeps the inner one only.
    fn mixes_nothings(&self, body: TyId, args: &[TyId]) -> bool {
        if !args.iter().any(|a| self.ty.nothing_of(*a).is_some()) {
            return false;
        }
        let mut stack = vec![body];
        let mut seen = std::collections::HashSet::new();
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            let k = self.ty.kind(t);
            if let TyKind::Option(p, n) = k {
                // The parameter itself, or as a member of a union (`T | string | null`).
                let members = match self.union_def(*p) {
                    Some((_, members)) => members,
                    None => vec![*p],
                };
                let direct = members.len() == 1;
                for m in members {
                    if let TyKind::Param(i) = self.ty.kind(m) {
                        let arg = args.get(*i as usize).and_then(|a| self.ty.nothing_of(*a));
                        // `P | undefined` at `P = T | null` is `T | null | undefined`.
                        let clash = |m: crate::hir::Nothing| {
                            if direct {
                                n.has_null() && m.has_undefined()
                            } else {
                                !m.within(*n)
                            }
                        };
                        if arg.is_some_and(clash) {
                            return true;
                        }
                    }
                }
            }
            stack.extend(crate::types::children(k));
        }
        false
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
            || self.brand_base(t).is_some()
            || matches!(self.ty.kind(t), TyKind::Adt(d, _)
                if self.adt(*d).is_some_and(|a| a.kind == crate::hir::AdtKind::Anon))
    }
}

/// Resolve every type alias that nothing expanded, so its errors are reported too
/// (`type X = Nope;` used nowhere is still an error, as in TypeScript). Aliases that are used
/// were checked at their uses.
pub(crate) fn check_unused_aliases(cx: &mut Ctx) {
    cx.checking_unused_aliases = true;
    for a in 0..cx.aliases.len() {
        if cx.aliases[a].used {
            continue;
        }
        let (module, decl) = (cx.aliases[a].module, cx.aliases[a].decl);
        let names: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
        cx.aliases[a].used = true;
        cx.aliases[a].expanding = true;
        cx.resolve_type(&decl.ty, &TyEnv::new(module, &names));
        cx.aliases[a].expanding = false;
    }
    cx.checking_unused_aliases = false;
}

/// The type written for an optional field `name?: T`: `T`, without the `| null` the parser adds
/// (`velt_syntax` `or_null`; the added `null` is the zero-width one at the end). A written type
/// that already admits `null` (`name?: T | null`) is kept as written.
pub(crate) fn written_type(ty: &ast::TypeExpr) -> ast::TypeExpr {
    let ast::TypeExprKind::Union(ms) = &ty.kind else {
        return ty.clone();
    };
    let added =
        |m: &ast::TypeExpr| matches!(m.kind, ast::TypeExprKind::Null) && m.span.lo == m.span.hi;
    match ms.last() {
        Some(last) if added(last) => {
            let rest = &ms[..ms.len() - 1];
            match rest {
                [one] => one.clone(),
                _ => ast::TypeExpr {
                    kind: ast::TypeExprKind::Union(rest.to_vec()),
                    span: ty.span,
                },
            }
        }
        _ => ty.clone(),
    }
}

/// The function type forms Velt parses but doesn't support yet (#943): its error and a note.
fn unsupported_fn_type(kind: &ast::TypeExprKind) -> (&'static str, &'static str) {
    match kind {
        ast::TypeExprKind::Construct(_) => (
            "construct signatures (`new (...) => T`) are not supported yet",
            "a class can't be passed as a value yet; pass a function that constructs it (`(buf: ArrayBuffer) => new Uint8Array(buf)`)",
        ),
        ast::TypeExprKind::GenericFunction { .. } => (
            "generic function types (`<T>(x: T) => R`) are not supported yet",
            "a function value has one type; declare a generic `function` and call it, or give the type parameters values (`(x: string) => R`)",
        ),
        _ => (
            "rest parameters in function types (`(...xs: T[]) => R`) are not supported yet",
            "take the array as an ordinary parameter (`(xs: T[]) => R`) and pass an array",
        ),
    }
}
