//! `new C(args)`: class instantiation (explicit or inferred type args; the constructor may be
//! inherited from a base class).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::ctx::Item;
use crate::defs::ThrowSrc;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(crate) fn new_expr(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if let Some(h) = self.mutex_new(class, args, exp, span) {
            return h;
        }
        if let Some(h) = self.promise_new(class, args, exp, span) {
            return h;
        }
        if self.bare_new_array(class, span) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let Some((d, slots)) = self.class_name(class) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let n = slots.len();
        let self_ty = crate::collect::self_type(self.cx, d, n);
        let ctor = self.cx.adt(d).and_then(|a| a.ctor);
        let cname = self.cx.adt(d).map(|a| a.name.clone()).unwrap_or_default();
        let (params, what) = match ctor {
            Some(c) => {
                let owner = self.cx.fn_info(c).owner.expect("ICE: ctor owner");
                let owner_ty = self.ancestor(self_ty, owner);
                let oargs = match self.cx.ty.kind(owner_ty) {
                    TyKind::Adt(_, a) => a.clone(),
                    _ => vec![],
                };
                crate::body::defaults::param_defaults(self.cx, c);
                let mut ps = self.cx.fn_info(c).params.clone();
                for p in &mut ps {
                    p.ty = self.cx.ty.subst(p.ty, &oargs);
                }
                (ps, format!("the constructor of `{cname}`"))
            }
            None => (vec![], format!("class `{cname}` (it has no constructor)")),
        };
        let names = self
            .cx
            .adt(d)
            .map(|a| a.generics.clone())
            .unwrap_or_default();
        let c = Callable {
            what,
            params,
            ret: self_ty,
            slot_names: names.names,
            bounds: names.bounds,
        };
        let ck = self.check_call(&c, slots, args, self.hint(exp), span);
        if let Some(c) = ctor {
            self.throw_src(ThrowSrc::Call(c, ck.type_args.clone(), span));
        }
        let kind = H::New {
            def: d,
            type_args: ck.type_args,
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    fn class_name(&mut self, t: &ast::TypeExpr) -> Option<(DefId, Vec<Option<TyId>>)> {
        let ast::TypeExprKind::Named { path, args } = &t.kind else {
            self.cx.err("expected a class name after `new`", t.span);
            return None;
        };
        let name = path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let item = self.lookup_type_path(path);
        let Some(Item::Def(d)) = item else {
            self.cx
                .err(format!("cannot find class `{name}` in this scope"), t.span);
            return None;
        };
        let Some(a) = self.cx.adt(d) else {
            self.cx.err(format!("`{name}` is not a class"), t.span);
            return None;
        };
        if a.kind != AdtKind::Class {
            self.cx.error(
                Diagnostic::error(format!("`{name}` is a struct, not a class"), t.span).with_note(
                    format!("create it with a struct literal: `{name} {{ ... }}`"),
                ),
            );
            return None;
        }
        let n = a.generics.len();
        if !args.is_empty() && args.len() != n {
            self.cx
                .err(format!("class `{name}` takes {n} type argument(s)"), t.span);
            return None;
        }
        let slots = if args.is_empty() {
            vec![None; n]
        } else {
            args.iter().map(|a| Some(self.resolve(a))).collect()
        };
        Some((d, slots))
    }

    /// The ancestor type of class type `t` whose def is `owner` (`t` itself if none).
    pub(crate) fn ancestor(&mut self, t: TyId, owner: DefId) -> TyId {
        let mut cur = t;
        for _ in 0..64 {
            if self.cx.class_of(cur).is_some_and(|(d, _)| d == owner) {
                return cur;
            }
            match self.cx.base_of(cur) {
                Some(b) => cur = b,
                None => break,
            }
        }
        t
    }
}
