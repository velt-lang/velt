//! `Partial<I>` of an interface with methods (sigx's `Request | Partial<ServerFnContext>`):
//! an object type with every member of `I` optional, as TypeScript's mapped type gives it. A
//! field or getter of `I` is an optional field of its type, and a method `m(x: A): R` an optional
//! field `m?: (x: A) => R`, the type `m?(x: A): R` written in an interface has. Its function type
//! is resolved from the method's declared signature, in the scope of the interface that declares
//! it, so a method without a `throws` clause is a function type without one, as written. A
//! generic method, which no function type of fixed types holds, is not supported yet; neither
//! is a method inherited from a generic interface.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// `Partial<t>` of interface value type `t` (module docs); `None` when `t` is no interface
    /// with methods.
    pub(crate) fn partial_iface(&mut self, t: TyId, module: usize, span: Span) -> Option<TyId> {
        match self.partial_fields(t)? {
            Ok(fields) => {
                let partial = self.anon_type_with(&fields, module);
                self.objects.partial_types.insert(partial, t);
                Some(partial)
            }
            Err(why) => {
                let shown = self.display(t);
                self.error(Diagnostic::error(
                    format!("`Partial<{shown}>` is not supported yet: {why}"),
                    span,
                ));
                Some(self.ty.error)
            }
        }
    }

    /// The fields of `Partial<t>` (module docs), or why it is not supported; `None` when `t`
    /// is no interface with methods.
    fn partial_fields(&mut self, t: TyId) -> Option<Result<Vec<ShapeField>, String>> {
        if !self.is_method_iface(t) {
            return None;
        }
        let TyKind::Dyn(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let mut fields = self.indexed_iface_members(t)?;
        let mut methods: Vec<String> = (self.iface(d)?.methods.iter())
            .filter(|m| {
                !m.is_getter && !m.name.starts_with("set ") && m.name != crate::generic_fns::CALL
            })
            .map(|m| m.name.clone())
            .collect();
        // The declared ones too: an alias of another module can be expanded before this
        // interface's methods are collected (a `Partial<I>` without them would be another type).
        for name in self.declared_methods(d, 0) {
            if !methods.contains(&name) {
                methods.push(name);
            }
        }
        for name in methods {
            match self.partial_method_ty(d, &args, &name) {
                Ok(ty) => fields.push(ShapeField {
                    name,
                    ty,
                    readonly: false,
                    optional: false,
                }),
                Err(why) => return Some(Err(why)),
            }
        }
        for f in &mut fields {
            f.optional = true;
        }
        Some(Ok(fields))
    }

    /// The interface `I` whose `Partial<I>` is `exp` or a member of union `exp`, and which a
    /// value of type `found` has: its own, one it extends, or one class `found` implements; or
    /// the same through an array's elements (`I[]` for `Partial<I>[]`) or a function's result.
    /// TypeScript converts every `I` to `Partial<I>` keeping the value; Velt's `Partial<I>` is an
    /// object type of its own, which the value would be copied into
    /// (`body/expr/coerce_mismatch.rs` reports it).
    pub(crate) fn partial_source(&mut self, exp: TyId, found: TyId) -> Option<TyId> {
        self.partial_source_at(exp, found, 0)
    }

    fn partial_source_at(&mut self, exp: TyId, found: TyId, depth: u32) -> Option<TyId> {
        if depth > 16 {
            return None;
        }
        let exp = self.ty.core(exp).unwrap_or(exp);
        let found = self.ty.core(found).unwrap_or(found);
        // `I[]` (or `readonly I[]`) where `Partial<I>[]` is expected.
        let (fe, ee) = (self.ty.mutable_twin(found), self.ty.mutable_twin(exp));
        if let (Some(f), Some(e)) = (self.ty.array_elem(fe), self.ty.array_elem(ee)) {
            return self.partial_source_at(e, f, depth + 1);
        }
        // `() => I` where `() => Partial<I>` is expected.
        if let (
            TyKind::FnPtr {
                params: fp,
                ret: fr,
                ..
            },
            TyKind::FnPtr {
                params: ep,
                ret: er,
                ..
            },
        ) = (self.ty.kind(found).clone(), self.ty.kind(exp).clone())
        {
            return match fp == ep {
                true => self.partial_source_at(er, fr, depth + 1),
                false => None,
            };
        }
        let targets = self.union_members(exp).unwrap_or_else(|| vec![exp]);
        if !targets
            .iter()
            .any(|t| self.objects.partial_types.contains_key(t))
        {
            return None;
        }
        let mut sources = vec![found];
        // The interfaces an interface value's type extends, at its arguments.
        let mut i = 0;
        while i < sources.len() && i < 64 {
            if let TyKind::Dyn(d, args) = self.ty.kind(sources[i]).clone() {
                let parents: Vec<_> = self.iface(d).map_or(vec![], |f| f.parents.clone());
                for p in parents {
                    let t = self.ty.intern(TyKind::Dyn(p.iface, p.args));
                    let t = self.subst(t, &args);
                    if !sources.contains(&t) {
                        sources.push(t);
                    }
                }
            }
            i += 1;
        }
        let mut class = self.class_of(found);
        for _ in 0..32 {
            let Some((c, args)) = class else { break };
            let Some(a) = self.adt(c) else { break };
            let (implements, base) = (a.implements.clone(), a.base);
            for b in implements {
                let t = self.ty.intern(TyKind::Dyn(b.iface, b.args));
                sources.push(self.subst(t, &args));
            }
            class = base
                .map(|b| self.subst(b, &args))
                .and_then(|b| self.class_of(b));
        }
        sources.into_iter().find(|&s| {
            self.partial_fields(s)
                .and_then(Result::ok)
                .is_some_and(|f| targets.contains(&self.anon_type_with(&f, 0)))
        })
    }

    /// The interface whose `Partial` is `exp`, a member of union `exp`, or `exp`'s payload when
    /// it may be null (`Partial<I> | undefined`): where a dynamic value can't convert yet
    /// (`body/expr/dynamic.rs` `unbox_dynamic`).
    pub(crate) fn partial_target(&mut self, exp: TyId) -> Option<TyId> {
        let core = self.ty.core(exp).unwrap_or(exp);
        let members = self.union_members(core).unwrap_or_else(|| vec![core]);
        members.into_iter().find_map(|m| {
            self.objects
                .partial_types
                .get(&self.ty.core(m).unwrap_or(m))
                .copied()
        })
    }

    /// Type `t` as written, each `Partial<I>` made for an interface with methods shown so
    /// (`Partial<Ctx>[]`, `() => Partial<Ctx>`, `Partial<Ctx> | undefined`), not as its object type.
    pub(crate) fn partial_shown(&mut self, t: TyId, depth: u32) -> String {
        if depth > 16 {
            return self.display(t);
        }
        if let Some(&i) = self.objects.partial_types.get(&t) {
            return format!("Partial<{}>", self.display(i));
        }
        if let Some(payload) = self.ty.opt_payload(t) {
            if self.partial_target(payload).is_some() {
                return format!("{} | undefined", self.partial_shown(payload, depth + 1));
            }
        }
        if self.ty.is_readonly_array(t) {
            let m = self.ty.mutable_twin(t);
            if let Some(e) = self.ty.array_elem(m) {
                return format!("readonly {}[]", self.partial_shown(e, depth + 1));
            }
        }
        match self.ty.kind(t).clone() {
            TyKind::Array(e) => format!("{}[]", self.partial_shown(e, depth + 1)),
            TyKind::FnPtr { params, ret, .. } => {
                let ps: Vec<String> = params.iter().map(|&p| self.display(p)).collect();
                format!(
                    "({}) => {}",
                    ps.join(", "),
                    self.partial_shown(ret, depth + 1)
                )
            }
            _ => match self.union_members(t) {
                Some(ms)
                    if ms
                        .iter()
                        .any(|m| self.objects.partial_types.contains_key(m)) =>
                {
                    let shown: Vec<String> = ms
                        .iter()
                        .map(|&m| self.partial_shown(m, depth + 1))
                        .collect();
                    shown.join(" | ")
                }
                _ => self.display(t),
            },
        }
    }

    /// The error for a value shown as `found` converted to `expected`, which holds
    /// `Partial<iface>` (module docs).
    pub(crate) fn partial_limit_error(
        &mut self,
        found: &str,
        expected: TyId,
        iface: TyId,
        span: Span,
    ) -> Diagnostic {
        let i = self.display(iface);
        let e = self.partial_shown(expected, 0);
        Diagnostic::error(
            format!("a value of type `{found}` cannot convert to `{e}` yet"),
            span,
        )
        .with_note(format!(
            "`Partial<{i}>` of an interface with methods is an object type of its own: the conversion would copy the members into a new object, where JavaScript keeps the one value (`===`, later writes)"
        ))
        .with_note(format!(
            "take `{i}` itself where a full value is passed, or build the `Partial<{i}>` as an object literal"
        ))
    }

    /// The function type of method `name` of interface `d` at `args`, from its declaration.
    fn partial_method_ty(&mut self, d: DefId, args: &[TyId], name: &str) -> Result<TyId, String> {
        let (owner, sig) = self
            .method_declaration(d, name, 0)
            .ok_or_else(|| {
                format!("method `{name}` comes from a type Velt made (`Pick`, `Omit` or an intersection of interfaces with methods), which has no written signature to make optional")
            })?;
        if !sig.generics.is_empty() {
            return Err(format!("method `{name}` is generic"));
        }
        if owner != d
            && self
                .iface(owner)
                .is_some_and(|i| !i.generics.names.is_empty())
        {
            return Err(format!(
                "method `{name}` comes from a generic interface it extends"
            ));
        }
        let span = sig.name.span;
        let ret = sig.ret.clone().unwrap_or(ast::TypeExpr {
            kind: ast::TypeExprKind::Void,
            span,
        });
        let fn_ty = ast::TypeExpr {
            kind: ast::TypeExprKind::Function {
                params: sig.params.iter().map(|p| p.ty.clone()).collect(),
                ret: Box::new(ret),
                throws: sig.throws.clone().map(Box::new),
                rest: sig.params.last().is_some_and(|p| p.rest),
            },
            span,
        };
        let info = self.iface(owner).expect("ICE: declaring interface");
        let env = TyEnv::new(info.module, &info.generics.names);
        let ty = self.resolve_type(&fn_ty, &env);
        Ok(match owner == d {
            true => self.subst(ty, args),
            false => ty,
        })
    }

    /// The names of the methods interface `d` and those it extends declare, as written
    /// (getters and setters left out).
    fn declared_methods(&self, d: DefId, depth: u32) -> Vec<String> {
        let Some(info) = self.iface(d).filter(|_| depth <= 32) else {
            return vec![];
        };
        let own = (info.decl.into_iter().flat_map(|x| x.methods.iter()))
            .filter(|m| !m.is_getter && !m.is_setter)
            .map(|m| m.sig.name.name.clone());
        let mut out: Vec<String> = own.collect();
        for p in &info.parents {
            for name in self.declared_methods(p.iface, depth + 1) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// The interface among `d` and those it extends that declares method `name`, and the
    /// method's signature as written.
    fn method_declaration(&self, d: DefId, name: &str, depth: u32) -> Option<(DefId, &ast::FnSig)> {
        if depth > 32 {
            return None;
        }
        let info = self.iface(d)?;
        if let Some(m) = (info.decl?.methods.iter())
            .find(|m| m.sig.name.name == name && !m.is_getter && !m.is_setter)
        {
            return Some((d, &m.sig));
        }
        info.parents
            .iter()
            .find_map(|p| self.method_declaration(p.iface, name, depth + 1))
    }
}
