//! The removed struct literal `Vec2 { x: 1 }` (#641): its error and the fix that rewrites it.

use velt_common::{Applicability, Diagnostic, Fix, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{DefId, TyId};

impl FnCx<'_, '_> {
    /// `Vec2 { x: 1 }` is no longer Velt (#641): an object literal builds a struct where its type
    /// is known (an annotation, a parameter, a return type, an element), or with `as`.
    pub(super) fn removed_struct_lit(
        &mut self,
        name: &ast::TypeExpr,
        d: Option<DefId>,
        shown: Option<String>,
        exp: Option<TyId>,
        span: Span,
    ) {
        let written = match &name.kind {
            ast::TypeExprKind::Named { path, .. } => path
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            _ => "T".to_string(),
        };
        let ty = shown.unwrap_or_else(|| written.clone());
        let known = self
            .hint(exp)
            .and_then(|t| self.adt_of(t))
            .is_some_and(|(a, _)| Some(a) == d);
        let fix = if known {
            Fix {
                title: format!("drop `{written}`: the type is known here"),
                edits: vec![(name.span, String::new())],
                applicability: Applicability::Safe,
            }
        } else {
            Fix {
                title: format!("write `{{ … }} as {ty}`"),
                edits: vec![
                    (name.span, "(".to_string()),
                    (Span::new(span.file, span.hi, span.hi), format!(" as {ty})")),
                ],
                applicability: Applicability::Safe,
            }
        };
        self.cx.error(
            Diagnostic::error(
                format!("`{written} {{ … }}` is not Velt: write an object literal where the type is known"),
                name.span,
            )
            .with_note(format!(
                "drop `{ty}` and annotate the target (`const v: {ty} = {{ … }}`), or write `{{ … }} as {ty}`"
            ))
            .with_fix(fix),
        );
    }
}
