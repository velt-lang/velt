//! Members of namespace imports (`import * as ns from "…"`): `ns.x` in an expression is read as
//! the module-level name `ns.x`, which `crate::collect::imports` binds to the module's export
//! `x`. Rewriting the syntax (rather than adding a namespace value) lets every form that works on
//! an imported name work through a namespace too: `ns.f()`, `ns.LIMIT`, `new ns.Class()`,
//! `ns.Enum.Member`, `ns.Class.staticMethod()`. A local named `ns` shadows the namespace.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;

impl FnCx<'_, '_> {
    /// Is `name` a namespace import of this module (not shadowed by a local)?
    pub(crate) fn is_namespace(&self, name: &str) -> bool {
        self.cx.scopes[self.module].namespaces.contains_key(name) && !self.is_local_name(name)
    }

    /// `ns.x` as the identifier `ns.x` (spanning `x`, so uses and errors point at the member).
    pub(crate) fn namespace_member(
        &self,
        object: &ast::Expr,
        prop: &ast::Ident,
    ) -> Option<ast::Ident> {
        let ast::ExprKind::Ident(ns) = &object.kind else {
            return None;
        };
        self.is_namespace(&ns.name).then(|| ast::Ident {
            name: format!("{}.{}", ns.name, prop.name),
            span: prop.span,
        })
    }

    /// `e` with its namespace prefix resolved: `ns.x` → `ns.x` (one identifier), `ns.E.y` →
    /// member `y` of that identifier; `None` if `e` does not start with a namespace.
    pub(crate) fn without_namespace(&self, e: &ast::Expr) -> Option<ast::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional,
        } = &e.kind
        else {
            return None;
        };
        let kind = match self.namespace_member(object, prop) {
            Some(id) if !optional => ast::ExprKind::Ident(id),
            Some(_) => return None,
            None => ast::ExprKind::Member {
                object: Box::new(self.without_namespace(object)?),
                prop: prop.clone(),
                optional: *optional,
            },
        };
        Some(ast::Expr {
            id: e.id,
            kind,
            span: e.span,
        })
    }

    /// The item a type path names where a value is created (`new ns.C()`, `ns.P { … }`).
    pub(crate) fn lookup_type_path(&mut self, path: &[ast::Ident]) -> Option<crate::ctx::Item> {
        match path {
            [name] => self.lookup_item(&name.name, name.span),
            [.., last] => {
                let item = self.cx.lookup_path_at(self.module, path, last.span);
                self.cx.rec_item(last.span, item);
                item
            }
            [] => None,
        }
    }

    /// The error for `ns.x` when namespace `ns` has no export `x`; false if `name` is not a
    /// namespace member.
    pub(crate) fn unknown_namespace_member(&mut self, name: &str, span: Span) -> bool {
        let Some((ns, member)) = name.split_once('.') else {
            return false;
        };
        if !self.cx.scopes[self.module].namespaces.contains_key(ns) {
            return false;
        }
        self.cx.err(
            format!("namespace `{ns}` has no exported member `{member}`"),
            span,
        );
        true
    }
}
