//! Rules on what a module imports: the Velt standard library (`velt-import`), files outside the
//! linted set (`outside-import`), and the default JSX provider (`jsx-provider`).

use std::path::Path;

use velt_common::Span;
use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

use super::Cx;
use crate::LintModule;

/// Check `module`'s imports and JSX provider; `scope` is the files being linted.
pub(super) fn check(module: &LintModule, scope: &[&Path], cx: &mut Cx) {
    for item in &module.ast.items {
        if let ast::ItemKind::Import(import) = &item.kind {
            import_item(module, import, scope, cx);
        }
    }
    if module.default_jsx_provider {
        jsx_provider(module.ast, cx);
    }
}

fn import_item(module: &LintModule, import: &ast::Import, scope: &[&Path], cx: &mut Cx) {
    let spec = import.from.as_str();
    if spec.starts_with("velt:") {
        cx.error(
            "velt-import",
            import.from_span,
            format!("`{spec}` is a Velt standard library module, which TypeScript can't import"),
            &[
                "`tsc` resolves `velt:` specifiers to nothing; only Velt provides these modules",
                "keep code that uses the Velt standard library out of the shared files, and \
                 pass what it computes in",
            ],
        );
    } else if spec.starts_with("./") || spec.starts_with("../") {
        let target = module.imports.iter().find(|(s, _)| s == spec);
        if let Some((_, file)) = target.filter(|(_, file)| !scope.contains(&file.as_path())) {
            outside_import(spec, file, import.from_span, cx);
        }
    }
}

fn outside_import(spec: &str, file: &Path, span: Span, cx: &mut Cx) {
    let lint_it = format!(
        "lint `{}` too (pass it to `velt check --ts-compat`), or keep the import out of shared \
         code",
        file.display()
    );
    cx.error(
        "outside-import",
        span,
        format!("`{spec}` imports a file outside the TypeScript-compatible files"),
        &[
            "`tsc` compiles every file a shared file imports, but this one isn't linted, so \
             it may use Velt-only code",
            &lint_it,
        ],
    );
}

/// One finding per module, at its first JSX element.
fn jsx_provider(module: &ast::Module, cx: &mut Cx) {
    let mut first = FirstJsx(None);
    visit::walk_module(module, &mut first);
    let Some(span) = first.0 else { return };
    cx.error(
        "jsx-provider",
        span,
        "this JSX uses `velt:jsx`, the default provider, which has no TypeScript runtime".into(),
        &[
            "`tsc` compiles JSX into calls of its `jsxImportSource`'s runtime; `velt:jsx` \
             exists only in Velt",
            "use a provider with both a Velt and a TypeScript runtime: `// @jsxImportSource \
             <package>` at the top of the file, or `jsx.importSource` in package.vlt",
        ],
    );
}

/// Finds the first JSX element or fragment.
struct FirstJsx(Option<Span>);

impl<'a> Visit<'a> for FirstJsx {
    fn expr(&mut self, e: &'a ast::Expr) {
        if self.0.is_none() && matches!(e.kind, ast::ExprKind::Jsx(_)) {
            self.0 = Some(e.span);
        }
    }
}
