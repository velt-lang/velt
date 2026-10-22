//! "mismatched types" for a function that throws where a function type that cannot throw is
//! expected, directly (`const r: (s: string) => string = import.meta.resolve`) or as an object's
//! field (`const m: { resolve: (s: string) => string } = import.meta`): a note with the ways out.
//! In a user module such a type written without `throws` takes the function's errors instead
//! (`throws::fn_types`); this is for the ones that can't (the standard library's).

use velt_common::Diagnostic;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// A function value that throws where a function type that doesn't is expected (`const r:
    /// (s: string) => string = import.meta.resolve`): says so, with the two ways out. `true`
    /// when reported.
    pub(super) fn throws_where_none_may(
        &mut self,
        expected: TyId,
        found: &hir::Expr,
        e: &str,
        f: &str,
    ) -> bool {
        let (
            TyKind::FnPtr {
                params: fp,
                ret: fr,
                throws: ft,
                rest: frest,
            },
            TyKind::FnPtr {
                params: ep,
                ret: er,
                throws: et,
                rest: erest,
            },
        ) = (
            self.cx.ty.kind(found.ty).clone(),
            self.cx.ty.kind(expected).clone(),
        )
        else {
            return false;
        };
        let never = self.cx.ty.never;
        if fp != ep || fr != er || frest != erest || ft == never || et != never {
            return false;
        }
        let thrown = self.cx.display(ft);
        // `(x: string) => import.meta.resolve(x)`: the arrow that wraps the value.
        let names: Vec<String> = (0..fp.len())
            .map(|i| match fp.len() {
                1 => "x".to_string(),
                _ => format!("x{i}"),
            })
            .collect();
        let typed: Vec<String> = names
            .iter()
            .zip(&fp)
            .map(|(n, &t)| format!("{n}: {}", self.cx.display(t)))
            .collect();
        let callee = self
            .wrapped_text(found)
            .filter(|t| !t.starts_with('<'))
            .unwrap_or_else(|| "f".to_string());
        let arrow = format!("({}) => {callee}({})", typed.join(", "), names.join(", "));
        self.cx.error(
            Diagnostic::error("mismatched types", found.span)
                .with_note(format!("expected {e}, found {f}"))
                .with_note(format!(
                    "the function throws `{thrown}`, and `{e}` cannot throw: write the type with `throws {thrown}`, or wrap it in an arrow, whose type says it throws: `const g = {arrow}`"
                )),
        );
        true
    }

    /// The source text of a local, a module constant or a field path of one
    /// (`import.meta.resolve`), for a suggestion.
    fn wrapped_text(&self, h: &hir::Expr) -> Option<String> {
        match &h.kind {
            H::Global(d) => self.cx.global(*d).map(|g| g.name.clone()),
            H::Field { base, index, .. } => {
                let TyKind::Adt(d, _) = self.cx.ty.kind(base.ty) else {
                    return None;
                };
                let field = self.cx.adt(*d)?.fields.get(*index as usize)?;
                Some(format!("{}.{}", self.wrapped_text(base)?, field.name))
            }
            _ => self.place_text(h),
        }
    }

    /// Why an object (or `import.meta`) of type `found` doesn't convert to object type
    /// `expected` when a function field throws where the expected field's type cannot (`const
    /// r: { resolve: (s: string) => string } = import.meta`), or `None`.
    pub(super) fn field_throws_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let (TyKind::Adt(fd, fargs), TyKind::Adt(ed, eargs)) = (
            self.cx.ty.kind(found).clone(),
            self.cx.ty.kind(expected).clone(),
        ) else {
            return None;
        };
        let (src, dst) = (
            self.cx.adt(fd)?.fields.clone(),
            self.cx.adt(ed)?.fields.clone(),
        );
        let never = self.cx.ty.never;
        for g in &dst {
            let Some(f) = src.iter().find(|f| f.name == g.name) else {
                continue;
            };
            let (fty, gty) = (self.cx.subst(f.ty, &fargs), self.cx.subst(g.ty, &eargs));
            let (
                TyKind::FnPtr {
                    params: fp,
                    ret: fr,
                    throws: ft,
                    rest: frest,
                },
                TyKind::FnPtr {
                    params: gp,
                    ret: gr,
                    throws: gt,
                    rest: grest,
                },
            ) = (self.cx.ty.kind(fty).clone(), self.cx.ty.kind(gty).clone())
            else {
                continue;
            };
            if fp == gp && fr == gr && frest == grest && ft != never && gt == never {
                let thrown = self.cx.display(ft);
                let want = self.cx.display(gty);
                return Some(format!(
                    "the function in field `{}` throws `{thrown}`, and `{want}` cannot throw: write the field's type with `throws {thrown}`",
                    g.name
                ));
            }
        }
        None
    }
}
