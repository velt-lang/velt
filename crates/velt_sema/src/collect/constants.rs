//! Constants: module-level `const` declarations and `static` fields of structs and
//! classes. Both become `Def::Global`s, checked on first use (`body::driver::ensure_global`);
//! a static field is named `Type.NAME` and keeps its declaring type for `private` checks. A
//! `static` field that is not `readonly` is module state, as a module-level `let` is: evaluated
//! once, where the class is declared, and assignable (`Type.count = 5`).

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::DefId;

/// `static [readonly] NAME: T = e;` fields: a module constant (or `let`) each, named `Type.NAME`.
pub(super) fn declare_statics<'m>(cx: &mut Ctx<'m>, m: usize, owner: DefId, t: &'m ast::TypeDecl) {
    for f in t.fields.iter().filter(|f| f.is_static) {
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
            mutable: !f.readonly,
            stored: !f.readonly,
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
        mutable: v.kind == ast::VarKind::Let,
        // A `const` moved out of an imported module's statements is kept as they would keep it
        // (`module_chunks`).
        stored: v.kind == ast::VarKind::Let || cx.promoted_consts.contains(&v.span),
    };
    Some((name, DefInfo::Global(Box::new(info))))
}
