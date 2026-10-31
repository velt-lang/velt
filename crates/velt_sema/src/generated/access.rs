//! Generated modules' scope and access (docs/internals/design/source-generators.md §7):
//!
//! - a generated module is checked in the scope of the module declaring the generated functions
//!   it implements: that module's imports and top-level declarations, exported or not, are
//!   visible in it as if it were appended to it (its own declarations come first). It is never a
//!   std module, so it has no compiler intrinsics and none of std's privileges;
//! - code generated for a type has that type's access: in a generated module, the `private`
//!   members of the declared types of its requests' models ([`super::GenModule::access`]) may be
//!   used as in their bodies, and `__construct<T>({ … })` builds such a class or struct without
//!   running its constructor (every field given, or left out to take its initializer, or absent
//!   when optional). ES private names (`#x`) stay private to their class, as in JavaScript.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::Ctx;
use crate::hir::{self, AdtKind, DefId};

/// The name of the constructor-free builder of generated code.
pub const CONSTRUCT: &str = "__construct";

/// Give generated modules `ms` the names of their declaring module's scope that they do not
/// declare themselves (after their own items are declared).
pub(crate) fn inherit_scopes(cx: &mut Ctx, ms: &[usize]) {
    let Some(gen) = cx.gen.clone() else {
        return;
    };
    for gm in &gen.modules {
        let index = |path: &str| cx.modules.iter().position(|m| m.path == path);
        let (Some(g), Some(d)) = (index(&gm.path), index(&gm.declaring)) else {
            continue;
        };
        if !ms.contains(&g) {
            continue;
        }
        let decl = &cx.scopes[d];
        let items: Vec<_> = decl.items.iter().map(|(n, i)| (n.clone(), *i)).collect();
        let namespaces: Vec<_> = (decl.namespaces.iter())
            .map(|(n, m)| (n.clone(), *m))
            .collect();
        let type_only: Vec<String> = decl.type_only.iter().cloned().collect();
        let via_export_type: Vec<String> = decl.via_export_type.iter().cloned().collect();
        let type_keys = decl.type_keys;
        let own = &mut cx.scopes[g];
        for (name, item) in items {
            if !own.items.contains_key(&name) {
                if type_only.contains(&name) {
                    own.type_only.insert(name.clone());
                }
                if via_export_type.contains(&name) {
                    own.via_export_type.insert(name.clone());
                }
                own.items.insert(name, item);
            }
        }
        for (name, m) in namespaces {
            own.namespaces.entry(name).or_insert(m);
        }
        own.type_keys |= type_keys;
    }
}

impl Ctx<'_> {
    /// Whether code of `module` has the access of type `owner`: `module` is a generated module
    /// whose requests' models hold `owner`.
    pub(crate) fn generated_access(&self, module: usize, owner: DefId) -> bool {
        let Some(gen) = self.gen.as_ref() else {
            return false;
        };
        let path = &self.modules[module].path;
        let Some(gm) = gen.modules.iter().find(|g| g.path == *path) else {
            return false;
        };
        self.adt(owner)
            .is_some_and(|a| gm.access.contains(&a.qual_name))
    }
}

impl FnCx<'_, '_> {
    /// `__construct<T>({ … })` in generated code: a `T` built from the fields given, without
    /// running its constructor.
    pub(crate) fn generated_construct(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if id.name != CONSTRUCT || !self.cx.in_generated_file(callee.span) {
            return None;
        }
        let ([t], [arg]) = (type_args, args) else {
            let msg = format!("`{CONSTRUCT}` takes one type argument and one object literal");
            self.cx.err(msg, span);
            return Some(self.error_expr(span));
        };
        let ast::ExprKind::Object(props) = &arg.kind else {
            let msg = format!("`{CONSTRUCT}` takes an object literal of the fields");
            self.cx.err(msg, arg.span);
            return Some(self.error_expr(span));
        };
        let ty = self.resolve(t);
        let target = self
            .adt_of(ty)
            .filter(|(d, _)| self.cx.adt(*d).is_some_and(|a| a.kind != AdtKind::Anon));
        let Some((d, targs)) = target else {
            let tn = self.cx.display(ty);
            let msg = format!("`{CONSTRUCT}` builds a class or a struct, not `{tn}`");
            self.cx.err(msg, t.span);
            return Some(self.error_expr(span));
        };
        if !self.cx.generated_access(self.module, d) {
            let tn = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(
                    format!("generated code cannot build `{tn}` without its constructor"),
                    t.span,
                )
                .with_note("only the types of the requests it was generated for"),
            );
            return Some(self.error_expr(span));
        }
        let slots = targs.into_iter().map(Some).collect();
        Some(self.fill_struct(d, slots, &[], props, None, span))
    }
}
