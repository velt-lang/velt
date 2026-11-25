//! `console.log("%j", x)` names `JSON` (#3662): sema writes the `%j` argument as
//! `JSON.stringify(x)` (velt_sema `body/expr/console_format.rs`, through std's `jsonText`), so a
//! module whose `console.*` call has a format string literal with a `%j` that takes an argument
//! loads the `JSON` global.

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

/// Whether `module` calls `console.<method>` with a string literal first whose `%j` takes one
/// of the arguments after it (`velt_sema::console_format::split`: not `%%j`).
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
            let Some((first, more)) = args.split_first().filter(|_| console) else {
                return;
            };
            let Some(fmt) = string_literal(first) else {
                return;
            };
            self.0 |= velt_sema::console_format::split(fmt, more.len()).is_some_and(|s| {
                (s.pieces.iter())
                    .any(|p| matches!(p, velt_sema::console_format::Piece::Arg(b'j', _)))
            });
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}

/// The text of `e` when it is a string literal (in parentheses or not).
fn string_literal(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s),
        ast::ExprKind::Paren(x) => string_literal(x),
        _ => None,
    }
}
