//! The errors of the rewrite ([`super`]): the capturing nested functions it keeps as declarations
//! (recursion it doesn't cover, a use before a captured local), and the decorated ones it refuses.

use velt_common::{Diagnostic, Span};

use super::{Cand, Rw};

impl Rw {
    pub(super) fn report_recursion(
        &mut self,
        cands: &[Cand],
        a: usize,
        path: &[usize],
        (why, fix): &(String, String),
    ) {
        let c = &cands[a];
        let through = match path {
            [] => String::new(),
            p => format!(
                " through {}",
                p.iter()
                    .map(|&b| format!("`{}`", cands[b].name.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        self.diags.push(
            Diagnostic::error(
                format!(
                    "`{}` uses local variables of the enclosing function and calls itself{through}: not supported yet",
                    c.name.name
                ),
                c.name.span,
            )
            .with_note(why.clone())
            .with_note(fix.clone()),
        );
    }

    pub(super) fn report_decorated(&mut self, c: &Cand, span: Span) {
        self.diags.push(
            Diagnostic::error(
                format!(
                    "`{}` uses its enclosing function's locals, so it takes no decorators",
                    c.name.name
                ),
                span,
            )
            .with_note("a function declared inside another that uses its locals is compiled as a closure, which a decorator would not reach")
            .with_note("declare it at the top level of its module, passing what it uses as parameters, or leave the decorator out"),
        );
    }

    pub(super) fn report_early_use(&mut self, c: &Cand, var: &str) {
        self.diags.push(
            Diagnostic::error(
                format!(
                    "`{}` is used before `{var}`, a local it uses, is declared: not supported yet",
                    c.name.name
                ),
                c.name.span,
            )
            .with_note("JS allows this when the function runs after `{var}` is set; declare `{var}` before the first use of the function".replace("{var}", var)),
        );
    }
}
