//! Test-only helpers for hand-building M2 HIR (types, ADTs, patterns, M2 expressions) the way
//! sema produces it (see hir.rs header).

use velt_sema::hir::*;

use super::builder::{ex, FB, PB, SP};

impl PB {
    pub fn ty(&mut self, k: TyKind) -> TyId {
        self.types.intern(k)
    }
    pub fn arr(&mut self, e: TyId) -> TyId {
        self.ty(TyKind::Array(e))
    }
    pub fn opt(&mut self, e: TyId) -> TyId {
        self.ty(TyKind::Option(e))
    }
    pub fn adt_ty(&mut self, d: DefId, args: Vec<TyId>) -> TyId {
        self.ty(TyKind::Adt(d, args))
    }
    pub fn param(&mut self, n: u32) -> TyId {
        self.ty(TyKind::Param(n))
    }
    pub fn fn_ty(&mut self, params: Vec<TyId>, ret: TyId) -> TyId {
        let throws = self.t.never;
        self.ty(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    pub fn add_def(&mut self, d: Def) -> DefId {
        let id = self.declare();
        self.defs[id.0 as usize] = Some(d);
        id
    }

    pub fn set_def(&mut self, id: DefId, d: Def) {
        self.defs[id.0 as usize] = Some(d);
    }

    pub fn add_impl(&mut self, i: ImplDef) {
        self.impls.push(i);
    }
}

/// A struct/class/anon definition.
pub(super) fn adt(name: &str, kind: AdtKind, fields: Vec<(&str, TyId, Option<Expr>)>) -> AdtDef {
    let is_copy = false;
    AdtDef {
        name: name.into(),
        kind,
        generics: 0,
        fields: fields
            .into_iter()
            .map(|(n, ty, default)| FieldDef {
                name: n.into(),
                ty,
                // Like sema: a `null` default is what an optional field (`a?: T`) gets.
                optional: matches!(
                    default,
                    Some(Expr {
                        kind: ExprKind::Lit(Lit::Null),
                        ..
                    })
                ),
                default,
                private: false,
                presence: false,
            })
            .collect(),
        is_copy,
        private_fields: false,
        opaque: false,
        assigned: false,
        base: None,
        ctor: None,
        dispose: None,
        vtable: vec![],
        span: SP,
    }
}

pub(super) fn enum_def(name: &str, variants: Vec<(&str, Vec<TyId>, i64)>) -> EnumDef {
    EnumDef {
        name: name.into(),
        generics: 0,
        variants: variants
            .into_iter()
            .map(|(n, payload, discriminant)| VariantDef {
                name: n.into(),
                payload,
                discriminant,
                str_value: None,
            })
            .collect(),
        is_copy: false,
        is_union: false,
        span: SP,
    }
}

impl FB {
    pub fn method(name: &str, self_ty: TyId, ret: TyId) -> Self {
        let mut f = FB::new(name, ret);
        f.self_ty = Some(self_ty);
        f
    }
}

fn bx(e: Expr) -> Box<Expr> {
    Box::new(e)
}

pub(super) fn field(base: Expr, index: u32, mode: UseMode, ty: TyId) -> Expr {
    ex(
        ExprKind::Field {
            base: bx(base),
            index,
            mode,
        },
        ty,
    )
}
pub(super) fn index(base: Expr, i: Expr, mode: UseMode, ty: TyId) -> Expr {
    ex(
        ExprKind::Index {
            base: bx(base),
            index: bx(i),
            mode,
        },
        ty,
    )
}
pub(super) fn adt_lit(def: DefId, fields: Vec<Expr>, ty: TyId) -> Expr {
    ex(
        ExprKind::AdtLit {
            def,
            type_args: vec![],
            fields,
        },
        ty,
    )
}
pub(super) fn variant(def: DefId, v: u32, args: Vec<Expr>, ty: TyId) -> Expr {
    ex(
        ExprKind::Variant {
            def,
            type_args: vec![],
            variant: v,
            args,
        },
        ty,
    )
}
pub(super) fn array(es: Vec<Expr>, ty: TyId) -> Expr {
    ex(ExprKind::ArrayLit(es), ty)
}
pub(super) fn new_obj(def: DefId, args: Vec<Expr>, ty: TyId) -> Expr {
    ex(
        ExprKind::New {
            def,
            type_args: vec![],
            args,
        },
        ty,
    )
}
pub(super) fn upcast(e: Expr, ty: TyId) -> Expr {
    ex(ExprKind::Upcast(bx(e)), ty)
}
pub(super) fn wrap_some(e: Expr, ty: TyId) -> Expr {
    ex(ExprKind::WrapSome(bx(e)), ty)
}
pub(super) fn unwrap_some(e: Expr, mode: UseMode, ty: TyId) -> Expr {
    ex(ExprKind::UnwrapSome(bx(e), mode), ty)
}
pub(super) fn null(ty: TyId) -> Expr {
    ex(ExprKind::Lit(Lit::Null), ty)
}
pub(super) fn to_dyn(e: Expr, impl_index: u32, ty: TyId) -> Expr {
    ex(
        ExprKind::ToDyn {
            expr: bx(e),
            impl_index,
        },
        ty,
    )
}
pub(super) fn throw(e: Expr, never: TyId) -> Expr {
    ex(ExprKind::Throw(bx(e)), never)
}
pub(super) fn closure(def: DefId, ty: TyId) -> Expr {
    ex(ExprKind::Closure(def), ty)
}
pub(super) fn fn_ref(def: DefId, ty: TyId) -> Expr {
    ex(ExprKind::FnRef(def, vec![]), ty)
}
pub(super) fn callee(c: Callee, args: Vec<Expr>, ty: TyId) -> Expr {
    ex(ExprKind::Call { callee: c, args }, ty)
}
pub(super) fn call_g(def: DefId, targs: Vec<TyId>, args: Vec<Expr>, ty: TyId) -> Expr {
    callee(Callee::Def(def, targs), args, ty)
}
pub(super) fn call_ptr(f: Expr, args: Vec<Expr>, ty: TyId) -> Expr {
    callee(Callee::Indirect(bx(f)), args, ty)
}
pub(super) fn match_(scrutinee: Expr, arms: Vec<(Pat, Option<Expr>, Expr)>, ty: TyId) -> Expr {
    ex(
        ExprKind::Match {
            scrutinee: bx(scrutinee),
            arms: arms
                .into_iter()
                .map(|(pat, guard, body)| Arm { pat, guard, body })
                .collect(),
        },
        ty,
    )
}

pub(super) fn pat(kind: PatKind, ty: TyId) -> Pat {
    Pat { kind, ty, span: SP }
}
pub(super) fn pbind(l: LocalId, mode: UseMode, ty: TyId) -> Pat {
    pat(PatKind::Binding(l, mode), ty)
}
pub(super) fn pwild(ty: TyId) -> Pat {
    pat(PatKind::Wildcard, ty)
}
pub(super) fn pvariant(def: DefId, variant: u32, args: Vec<Pat>, ty: TyId) -> Pat {
    pat(PatKind::Variant { def, variant, args }, ty)
}

pub(super) fn for_of(binding: Pat, iter: Expr, body: Vec<Stmt>) -> Stmt {
    super::builder::st(StmtKind::ForOf {
        label: None,
        binding,
        iter,
        body: super::builder::block(body),
        consume: false,
    })
}
pub(super) fn let_pat(p: Pat, init: Expr) -> Stmt {
    super::builder::st(StmtKind::LetPat { pat: p, init })
}
pub(super) fn try_(
    body: Vec<Stmt>,
    catch: Option<(Option<LocalId>, Vec<Stmt>)>,
    finally: Option<Vec<Stmt>>,
) -> Stmt {
    super::builder::st(StmtKind::Try {
        body: super::builder::block(body),
        catch: catch.map(|(l, b)| (l, super::builder::block(b))),
        finally: finally.map(super::builder::block),
    })
}

/// A closure def: `caps` are (outer local, inner name, type, mode); `params` follow them.
pub(super) fn closure_def(
    pb: &mut PB,
    name: &str,
    ret_ty: TyId,
    caps: &[(LocalId, TyId, PassMode)],
    params: &[(&str, TyId)],
    body: impl FnOnce(&FB, &[LocalId], &[LocalId]) -> Vec<Stmt>,
) -> DefId {
    let mut f = FB::new(name, ret_ty);
    let mut inner = vec![];
    for (i, (outer, ty, mode)) in caps.iter().enumerate() {
        let l = f.param(&format!("cap{i}"), *ty, *mode);
        f.captures.push(Capture {
            outer: *outer,
            inner: l,
            mode: *mode,
            share: false,
        });
        inner.push(l);
    }
    let ps: Vec<_> = params
        .iter()
        .map(|(n, ty)| f.param(n, *ty, PassMode::Borrow))
        .collect();
    let stmts = body(&f, &inner, &ps);
    pb.add_fn(f.build(stmts))
}
