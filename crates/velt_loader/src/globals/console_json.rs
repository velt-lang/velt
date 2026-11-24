//! `console.log("%j", x)` names `JSON` (#3662): sema writes the `%j` argument as
//! `JSON.stringify(x)` (velt_sema `body/expr/console_format.rs`), so a module whose `console.*`
//! call has a format string literal holding `%j` loads the `JSON` global.

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

/// Whether `module` calls `console.<method>` with a string literal holding `%j` first.
pub(super) fn has_console_json(module: &ast::Module) -> bool {
    struct Find(bool);
    impl<'a> Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            let ast::ExprKind::Call { callee, args, .. } = &e.kind else {
                return;
            };
            let ast::ExprKind::Member { object, .. } = &callee.kind else {
                return;
            };
            let console = matches!(&object.kind, ast::ExprKind::Ident(id) if id.name == "console");
            let json = matches!(
                args.first().map(|a| &a.kind),
                Some(ast::ExprKind::Lit(ast::Lit::Str(s))) if s.contains("%j")
            );
            self.0 |= console && json && args.len() > 1;
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}
