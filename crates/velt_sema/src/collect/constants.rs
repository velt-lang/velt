//! Constants: module-level `const` declarations and `static readonly` fields of structs and
//! classes. Both become `Def::Global`s, checked on first use (`body::driver::ensure_global`);
//! a static field is named `Type.NAME` and keeps its declaring type for `private` checks.

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::DefId;

/// `static readonly NAME: T = e;` fields: a module constant each, named `Type.NAME`.
pub(super) fn declare_statics<'m>(cx: &mut Ctx<'m>, m: usize, owner: DefId, t: &'m ast::TypeDecl) {
    for f in t.fields.iter().filter(|f| f.is_static) {
        if !f.readonly {
            cx.error(
                Diagnostic::error("static fields must be `readonly`", f.name.span)
                    .with_note("write `static readonly` (mutable statics are not supported)"),
            );
        }
        let owner_qual = cx
            .adt(owner)
            .map(|a| a.qual_name.clone())
            .unwrap_or_default();
        let qual = format!("{owner_qual}.{}", f.name.name);
        let info = GlobalInfo {
            name: format!("{}.{}", t.name.name, f.name.name),
            qual_name: qual,
            module: m,
            span: f.name.span,
            ty: cx.ty.error,
            init: None,
            state: BodyState::Unchecked,
            src: GlobalSrc {
                ann: Some(&f.ty),
                init: f.default.as_ref(),
                span: f.span,
                owner: Some(owner),
                is_private: f.is_private,
            },
        };
        let g = cx.alloc_def(f.name.span, DefInfo::Global(Box::new(info)));
        if cx
            .adt_mut(owner)
            .statics
            .insert(f.name.name.clone(), g)
            .is_some()
        {
            cx.err(
                format!("static field `{}` is declared more than once", f.name.name),
                f.name.span,
            );
        }
    }
}

pub(super) fn global_info<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    v: &'m ast::VarDecl,
) -> Option<(&'m ast::Ident, DefInfo<'m>)> {
    if v.kind == ast::VarKind::Let {
        // Reported once; the binding is then checked as a constant so its uses don't cascade.
        cx.error(
            Diagnostic::error("mutable module-level state is not allowed", v.span)
                .with_note("use `const` for a constant value")
                .with_note(
                    "for state that changes, create it in `main` (e.g. `const hits = shared(0);`) \
                     and pass it where it is needed",
                ),
        );
    }
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        cx.err(
            "destructuring is not supported in module-level constants",
            v.pattern.span,
        );
        return None;
    };
    let info = GlobalInfo {
        name: name.name.clone(),
        qual_name: cx.qualify(m, &name.name),
        module: m,
        span: name.span,
        ty: cx.ty.error,
        init: None,
        state: BodyState::Unchecked,
        src: GlobalSrc {
            ann: v.ty.as_ref(),
            init: v.init.as_ref(),
            span: v.span,
            owner: None,
            is_private: false,
        },
    };
    Some((name, DefInfo::Global(Box::new(info))))
}
