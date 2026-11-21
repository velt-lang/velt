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
                Err(why) => {
                    let shown = self.display(t);
                    self.error(Diagnostic::error(
                        format!("`Partial<{shown}>` is not supported yet: {why}"),
                        span,
                    ));
                    return Some(self.ty.error);
                }
            }
        }
        for f in &mut fields {
            f.optional = true;
        }
        Some(self.anon_type_with(&fields, module))
    }

    /// The function type of method `name` of interface `d` at `args`, from its declaration.
    fn partial_method_ty(&mut self, d: DefId, args: &[TyId], name: &str) -> Result<TyId, String> {
        let (owner, sig) = self
            .method_declaration(d, name, 0)
            .ok_or_else(|| format!("the declaration of method `{name}` is not found"))?;
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
