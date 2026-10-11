//! Destructured parameters (`function f([a, b]: [string, number])`, `xs.map(([k, v]) => …)`):
//! the parameter itself is a local no source can name (`#param0`, `ast::Param::pattern`), and
//! the body starts by destructuring it, exactly as `let [a, b] = #param0;` written there would.
//! So a destructured parameter costs what destructuring by hand costs, and its bindings follow
//! the rules of `let` bindings (ownership, cells for closures that assign them, defaults).

use velt_syntax::ast;

use super::FnCx;
use crate::hir;

impl FnCx<'_, '_> {
    /// `let <pattern> = <name>;` for each destructured parameter (its name, pattern, and
    /// whether it is optional), appended to `out` (after the parameters are declared, before
    /// the body). A parameter that may be `null` (`{ a }: P | null`) is an error, as destructuring
    /// any such value is.
    pub(crate) fn destructure_params<'p>(
        &mut self,
        params: impl IntoIterator<Item = (&'p ast::Ident, Option<&'p ast::Pattern>, bool)>,
        out: &mut Vec<hir::Stmt>,
    ) {
        for (name, pattern, optional) in params {
            let Some(pattern) = pattern else { continue };
            let mut init = ast::Expr {
                id: pattern.id,
                kind: ast::ExprKind::Ident(name.clone()),
                span: name.span,
            };
            if optional {
                // TS2463; `{ a }?: P` is `{ a }: P | null = null`, destructured as `P` from here
                // on so that only this is reported.
                self.cx.error(
                    velt_common::Diagnostic::error(
                        "a destructured parameter cannot be optional (TypeScript's TS2463)",
                        pattern.span,
                    )
                    .with_note("give it a default (`{ a }: P = { a: 0 }`), or take `p?: P` and check it for `null` in the body"),
                );
                init = ast::Expr {
                    id: pattern.id,
                    kind: ast::ExprKind::NonNull(Box::new(init)),
                    span: name.span,
                };
            }
            let decl = ast::Stmt {
                kind: ast::StmtKind::Var(ast::VarDecl {
                    kind: ast::VarKind::Let,
                    pattern: pattern.clone(),
                    ty: None,
                    init: Some(init),
                    span: pattern.span,
                }),
                span: pattern.span,
            };
            self.stmt(&decl, out);
        }
    }
}
