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
            Ok(fields) => Some(self.anon_type_with(&fields, module)),
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
        let methods: Vec<String> = (self.iface(d)?.methods.iter())
            .filter(|m| {
                !m.is_getter && !m.name.starts_with("set ") && m.name != crate::generic_fns::CALL
            })
            .map(|m| m.name.clone())
            .collect();
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

    /// The interface `I`, among `found`'s own and those class `found` implements, whose
    /// `Partial<I>` is `exp` or a member of union `exp`. TypeScript converts every `I` to
    /// `Partial<I>` keeping the value; Velt's `Partial<I>` is an object type of its own, which
    /// the value would be copied into (`body/expr/coerce_mismatch.rs` reports it).
    pub(crate) fn partial_source(&mut self, exp: TyId, found: TyId) -> Option<TyId> {
        let exp = self.ty.core(exp).unwrap_or(exp);
        let targets = self.union_members(exp).unwrap_or_else(|| vec![exp]);
        let found = self.ty.core(found).unwrap_or(found);
        let mut sources = vec![found];
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
