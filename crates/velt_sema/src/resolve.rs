//! Written types (`ast::TypeExpr`) → interned `TyId`s.
//!
//! Names resolve in order: generic parameters in scope, primitives, builtin generic types
//! (`Array<T>`, `Result<T, E>`, `Promise<T>`, `shared<T>`), then module items (own, imported,
//! prelude); `ns.T` names export `T` of namespace import `ns`. Interfaces used as types are
//! `TyKind::Dyn`; `T | null` is `TyKind::Option`; other unions are compiler-generated enums (`crate::unions`); literal types are `TyKind::Literal`
//! (`crate::literals`) and object types `{ a: T }` anonymous object types (`crate::anon`).

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
            this_ty: None,
        }
    }
}

impl Ctx<'_> {
    /// Resolve a written type. Reports and returns `Error` for unknown / invalid types.
    pub fn resolve_type(&mut self, t: &ast::TypeExpr, env: &TyEnv) -> TyId {
        use ast::TypeExprKind as K;
        let form = matches!(
            t.kind,
            K::Indexed { .. } | K::KeyOf(_) | K::Conditional(_) | K::Mapped(_)
        );
        if form && self.inline_deferrable(env) {
            return self.resolve_inline(t, env, |cx| cx.resolve_form(t, env));
        }
        self.resolve_form(t, env)
    }

    fn resolve_form(&mut self, t: &ast::TypeExpr, env: &TyEnv) -> TyId {
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
                rest,
            } => self.resolve_fn_type(params, *rest, ret, throws.as_deref(), env, t.span),
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
            ast::TypeExprKind::Construct(inner) => self.resolve_construct(inner, t, env),
            ast::TypeExprKind::GenericFunction { type_params, ty } => {
                self.resolve_generic_fn(type_params, ty, env, t.span)
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

    /// `{ a: A; b: B }`: the anonymous object type of that shape.
    pub(crate) fn resolve_object(&mut self, fields: &[ast::ObjectTypeField], env: &TyEnv) -> TyId {
        let mut out: Vec<ShapeField> = vec![];
        let mut spans = vec![];
        for f in fields {
            // The declared type: without the `| undefined` the parser adds for `name?: T`
            // (`velt_syntax` `or_undefined`), which the flag stands for.
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
        // `bigint` has no values until `BigInt` exists (#1088): `never`, so `string | bigint` is
        // `string` and `T extends bigint ? …` is never taken, as for every Velt value in TS.
        let bigint = (name == "bigint").then(|| self.ty.bigint());
        if let Some(p) = bigint.or_else(|| {
            self.ty
                .primitive(name)
                .filter(|_| !simd_outside_std && !shadowed)
        }) {
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
        if let (Item::Def(d), [buffer]) = (item, args) {
            if self.is_typed_array(d) {
                return self.typed_array_with_buffer(d, name, buffer, t, env);
            }
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
        let args = match item {
            Item::Alias(a) if !self.aliases[a as usize].outer.is_empty() => {
                let Some(mut outer) = self.outer_alias_args(a, env) else {
                    let n = &self.aliases[a as usize].outer[0];
                    self.err(format!("cannot find type `{n}` in this scope"), t.span);
                    return self.ty.error;
                };
                outer.extend(args);
                outer
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
            n if crate::utility_types::OPERATORS.contains(&n) && self.inline_deferrable(env) => {
                return Some(
                    self.resolve_inline(t, env, |cx| cx.resolve_utility(t, name, args, env)),
                )
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

    pub(crate) fn arity_error(&mut self, name: &str, want: usize, got: usize, t: &ast::TypeExpr) {
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

    /// Is `d` a standard typed array class or `DataView`?
    fn is_typed_array(&self, d: crate::hir::DefId) -> bool {
        self.std_class(d, TYPED_ARRAYS)
    }

    /// Is `d` a class of the standard library named one of `names`?
    fn std_class(&self, d: crate::hir::DefId, names: &[&str]) -> bool {
        match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => self.scopes[a.module].is_std && names.contains(&a.name.as_str()),
            _ => false,
        }
    }

    /// Typed array or `DataView` class `d` written with a buffer type argument (TypeScript
    /// 5.7's `Uint8Array<ArrayBuffer>`). Its buffer is always an `ArrayBuffer`, so the argument
    /// says nothing more: `ArrayBuffer`, or `ArrayBufferLike` (TypeScript's default, which
    /// without `SharedArrayBuffer` can only be an `ArrayBuffer`). Any other argument is an
    /// error; one that did not resolve was reported already.
    fn typed_array_with_buffer(
        &mut self,
        d: crate::hir::DefId,
        name: &str,
        buffer: &ast::TypeExpr,
        t: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        // A global name the program does not declare itself (`ArrayBufferLike`, and
        // `SharedArrayBuffer`, which Velt does not have).
        let global = match &buffer.kind {
            ast::TypeExprKind::Named { path, args } => match path.as_slice() {
                [p] if args.is_empty()
                    && self
                        .lookup_item_at(env.module, &p.name, buffer.span)
                        .is_none() =>
                {
                    Some(p.name.as_str())
                }
                _ => None,
            },
            _ => None,
        };
        if global != Some("ArrayBufferLike") {
            let shown = match global {
                Some(n @ "SharedArrayBuffer") => n.to_string(),
                _ => {
                    let b = self.resolve_type(buffer, env);
                    if b == self.ty.error {
                        return self.ty.error;
                    }
                    let is_buffer = matches!(self.ty.kind(b), TyKind::Adt(bd, bargs)
                        if bargs.is_empty() && self.std_class(*bd, &["ArrayBuffer"]));
                    if is_buffer {
                        return self.def_type(d, name, vec![], t);
                    }
                    self.display(b)
                }
            };
            self.error(
                velt_common::Diagnostic::error(
                    "a typed array's buffer type argument must be `ArrayBuffer`",
                    buffer.span,
                )
                .with_note(format!(
                    "found `{shown}`; `{name}<ArrayBuffer>` and `{name}<ArrayBufferLike>` are `{name}`, whose buffer is always an `ArrayBuffer` (`SharedArrayBuffer` is not supported)"
                )),
            );
            return self.ty.error;
        }
        self.def_type(d, name, vec![], t)
    }

    /// Does `body` (an alias's type, in terms of its parameters) hold a `P | null` or
    /// `P | undefined` of a parameter whose argument in `args` allows the other nothing? The
    /// substitution would make an option of an option, which keeps the inner one only.
    pub(crate) fn mixes_nothings(&self, body: TyId, args: &[TyId]) -> bool {
        if !args.iter().any(|a| self.ty.nothing_of(*a).is_some()) {
            return false;
        }
        let mut stack = vec![body];
        let mut seen = rustc_hash::FxHashSet::default();
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
    pub(crate) fn is_structural(&self, t: TyId) -> bool {
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
        let mut names: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
        cx.aliases[a].used = true;
        cx.aliases[a].expanding = true;
        // An alias of a generic function type that nothing uses as a type is fine: constants
        // of it are generic functions (`generic_arrows::typed`). Its function type is checked
        // with the type parameters in scope; a use elsewhere reports the form.
        let ty = match &decl.ty.kind {
            ast::TypeExprKind::GenericFunction { type_params, ty } => {
                names.extend(type_params.iter().map(|g| g.name.name.clone()));
                &**ty
            }
            _ => &decl.ty,
        };
        cx.resolve_type(ty, &TyEnv::new(module, &names));
        cx.aliases[a].expanding = false;
    }
    cx.checking_unused_aliases = false;
}

/// The type written for an optional field `name?: T`: `T`, without the `| undefined` the parser
/// adds (`velt_syntax` `or_undefined`; the added `undefined` is the zero-width one at the end).
/// A written type that already admits `undefined` (`name?: T | undefined`) is kept as written.
pub(crate) fn written_type(ty: &ast::TypeExpr) -> ast::TypeExpr {
    let ast::TypeExprKind::Union(ms) = &ty.kind else {
        return ty.clone();
    };
    let added = |m: &ast::TypeExpr| {
        matches!(m.kind, ast::TypeExprKind::Undefined) && m.span.lo == m.span.hi
    };
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

/// The standard typed arrays and `DataView`, which take TypeScript's buffer type argument.
const TYPED_ARRAYS: &[&str] = &[
    "Uint8Array",
    "Int8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float32Array",
    "Float64Array",
    "DataView",
];
