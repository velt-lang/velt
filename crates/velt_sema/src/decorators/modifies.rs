//! `@modifies("w")` on std's `declare generated` functions (std/lang/intrinsics/decorators.velt,
//! the hook `decorator_modifies`; #3543): the parameters every instance the generator writes
//! modifies, the generator's output or input stream (`writeJson`'s `w`, `readJson`'s `r`).
//!
//! A generated function and its instances share one ABI (`crate::generated`), so the ownership
//! passes join their modes (`ownership::mutation`'s groups). Without the decorator that join
//! raises the declaration when a program's instances are checked; in a check by units the
//! declaration is std's, finished and frozen by then (`crate::units`). With it, [`apply`] gives
//! the listed parameters their final mode (`BorrowMut`) where the function is declared, so no
//! instance raises them. The list is a floor, not the whole answer: an instance also runs the
//! program's own code (a `toJSON` that changes its object makes `writeJson`'s instance modify
//! `value`), so an unlisted parameter may still be raised, by the program, as any other mode
//! (`units::raised` keeps it beside a frozen declaration).

use velt_common::{std_hook, Diagnostic};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::{DefId, PassMode};

/// The `declare decorator function modifies`, when its module is loaded.
fn role(cx: &Ctx) -> Option<DefId> {
    match cx.std_def(std_hook!(decorator_modifies)) {
        Some(crate::ctx::Item::Def(d)) => Some(d),
        _ => None,
    }
}

/// Run after signatures, for the definitions from `from`: each function decorated `@modifies`
/// is checked to be one of std's `declare generated` functions, and the parameters it lists
/// get mode `BorrowMut`. A definition without decorators costs one look.
pub(crate) fn apply(cx: &mut Ctx, from: u32) {
    let mut want: Option<Option<DefId>> = None;
    for i in from as usize..cx.info.len() {
        let DefInfo::Fn(f) = &cx.info[i] else {
            continue;
        };
        let Some(FnSource::Decl(decl)) = f.source else {
            continue;
        };
        if decl.sig.decorators.is_empty() {
            continue;
        }
        let m = f.module;
        let want = *want.get_or_insert_with(|| role(cx));
        let Some(want) = want else { return };
        let uses: Vec<&ast::Decorator> = (decl.sig.decorators.iter())
            .filter(|dec| super::known::written_def(cx, m, dec) == Some(want))
            .collect();
        let Some(first) = uses.first() else {
            continue;
        };
        if !cx.scopes[m].is_std || f.generator.is_none() {
            let at = first.span;
            cx.error(
                Diagnostic::error("`@modifies` is only for std's generated functions", at)
                    .with_note("it says which parameters every instance of a `declare generated` function changes"),
            );
            continue;
        }
        let names: Vec<(String, velt_common::Span)> = (uses.iter())
            .flat_map(|dec| match &dec.expr.kind {
                ast::ExprKind::Call { args, .. } => args.iter().collect::<Vec<_>>(),
                _ => vec![],
            })
            .filter_map(|a| match &a.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => Some((s.clone(), a.span)),
                _ => None,
            })
            .collect();
        let params: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
        let mut listed = vec![];
        for (name, at) in names {
            match params.iter().position(|p| *p == name) {
                Some(k) => listed.push(k),
                None => cx.error(Diagnostic::error(
                    format!("`@modifies`: the function has no parameter `{name}`"),
                    at,
                )),
            }
        }
        if let DefInfo::Fn(f) = &mut cx.info[i] {
            for k in listed {
                f.params[k].mode = PassMode::BorrowMut;
            }
        }
    }
}
