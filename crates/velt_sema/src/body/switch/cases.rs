//! What a `switch` matches on (the cases are in `select`).
//!
//! The scrutinee is one of: the discriminant of a discriminated union (`switch (s.kind)`: the
//! union value `s` is matched, each case selects the members whose discriminant has that
//! literal), `typeof x` (each case selects the members with that tag), a union value (literal
//! cases select their member), an enum (`case Dir.Up:` selects the member) or a plain value
//! (literal patterns). The first four are described by *slots* — the members (and `null`) the
//! value can be — which drive narrowing of the case bodies and the exhaustiveness check.

use velt_syntax::ast;

use crate::body::narrow::literal_of;
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::hir::{self, DefId, LitValue, LocalId, PatKind as P, TyId, TyKind};
use crate::literals::display_lit;

/// How cases select values of the scrutinee.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ScrutKind {
    /// `switch (s.kind)` on a discriminated union: the discriminant of each member (variant order).
    Discriminant(Vec<LitValue>),
    /// `switch (typeof x)`.
    TypeOf,
    /// A union value (literal cases).
    Union,
    /// An enum value (`case E.Member:`).
    Enum(DefId),
    /// Any other value: literal patterns or `==` guards.
    Plain,
}

/// One possible member of the scrutinee (or `null`).
pub(super) struct Slot {
    pub pat: hir::Pat,
    /// Union variant (`None`: the `null` slot, or the only member of a non-union value).
    pub variant: Option<u32>,
    /// The member's type (`None` for `null`).
    pub member: Option<TyId>,
    /// How a missing case for this slot is shown (`"rect"`, `Dir.Up`, `null`).
    pub name: String,
}

pub(super) struct Scrut {
    /// The value matched: a place (a temporary is stored in a local first).
    pub expr: hir::Expr,
    pub kind: ScrutKind,
    /// The local that case bodies narrow (a union / nullable local, or the object of `x.kind`).
    pub local: Option<LocalId>,
    pub slots: Vec<Slot>,
    /// Some variants were ruled out by flow narrowing (they have no slot).
    pub partial: bool,
    /// The discriminant expression as written, for messages.
    pub what: String,
}

impl FnCx<'_, '_> {
    /// Check the `switch` discriminant; statements storing a temporary go to `pre`.
    pub(super) fn scrutinee(&mut self, disc: &ast::Expr, pre: &mut Vec<hir::Stmt>) -> Scrut {
        let what = source_text(disc);
        match &strip_parens(disc).kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if !self.is_type_name(object) => {
                let obj = self.expr(object, None, Want::Borrow);
                if let Some(values) = self.cx.discriminant_values(obj.ty, &prop.name) {
                    self.rec_discriminant(prop, obj.ty);
                    let local = self.named_local(object);
                    return self.scrut(obj, ScrutKind::Discriminant(values), local, what, pre);
                }
                let h = self.member_of(obj, prop, Want::Borrow, disc.span);
                self.value_scrut(h, None, what, pre)
            }
            ast::ExprKind::Unary {
                op: ast::UnaryOp::TypeOf,
                expr,
            } => {
                let x = self.expr(expr, None, Want::Borrow);
                let local = self.named_local(expr);
                self.scrut(x, ScrutKind::TypeOf, local, what, pre)
            }
            _ => {
                let h = self.expr(disc, None, Want::Borrow);
                let local = self.named_local(disc);
                self.value_scrut(h, local, what, pre)
            }
        }
    }

    /// For editors: the discriminant refers to the field of the union's first member.
    pub(crate) fn rec_discriminant(&mut self, prop: &ast::Ident, u: TyId) {
        let Some(m) = self.cx.union_members(u).and_then(|ms| ms.first().copied()) else {
            return;
        };
        if let (Some((d, _)), Some((i, _))) = (self.adt_of(m), self.cx.field_of(m, &prop.name)) {
            self.cx
                .rec_ref(prop.span, crate::ide::record::Target::Field(d, i));
        }
    }

    /// A value scrutinee: a union, an enum or a plain value.
    fn value_scrut(
        &mut self,
        h: hir::Expr,
        local: Option<LocalId>,
        what: String,
        pre: &mut Vec<hir::Stmt>,
    ) -> Scrut {
        // A branded value is compared as its primitive (`case "admin":` on a `UserId`).
        let h = self.unbrand(h);
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let kind = if self.cx.union_def(inner).is_some() {
            ScrutKind::Union
        } else {
            match self.cx.ty.kind(h.ty) {
                TyKind::Adt(d, _) if self.cx.enum_info(*d).is_some() => ScrutKind::Enum(*d),
                _ => ScrutKind::Plain,
            }
        };
        self.scrut(h, kind, local, what, pre)
    }

    fn scrut(
        &mut self,
        h: hir::Expr,
        kind: ScrutKind,
        local: Option<LocalId>,
        what: String,
        pre: &mut Vec<hir::Stmt>,
    ) -> Scrut {
        let expr = self.place_scrutinee(h, pre);
        let (slots, partial) = self.slots(&expr, &kind);
        Scrut {
            expr,
            kind,
            local,
            slots,
            partial,
            what,
        }
    }

    /// `h` itself if it is a place, else a fresh local holding it (a `Let` in `pre`).
    fn place_scrutinee(&mut self, h: hir::Expr, pre: &mut Vec<hir::Stmt>) -> hir::Expr {
        if is_place(&h) || self.cx.ty.is_bottom(h.ty) {
            return h;
        }
        let (ty, span) = (h.ty, h.span);
        let l = self.new_local("<switch>", ty, false, span, LocalKind::Temp);
        self.note_inferred_local(l, &h);
        pre.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span,
        });
        let mode = self.use_mode(ty, Want::Borrow);
        self.mk(hir::ExprKind::Local(l, mode), ty, span)
    }

    /// The slots of a scrutinee (and whether narrowing ruled out some variants).
    fn slots(&mut self, s: &hir::Expr, kind: &ScrutKind) -> (Vec<Slot>, bool) {
        let span = s.span;
        match kind {
            // A nullable plain value has a `null` slot and a value slot, so `default` after
            // `case null` sees the value non-null.
            ScrutKind::Plain if self.cx.ty.opt_payload(s.ty).is_some() => {
                self.member_slots(s, kind)
            }
            ScrutKind::Plain => (vec![], false),
            ScrutKind::Enum(d) => {
                let names: Vec<String> = self
                    .cx
                    .enum_info(*d)
                    .map(|e| {
                        e.variants
                            .iter()
                            .map(|v| format!("{}.{}", e.name, v.name))
                            .collect()
                    })
                    .unwrap_or_default();
                let slots = names
                    .into_iter()
                    .enumerate()
                    .map(|(i, name)| Slot {
                        pat: self.pat(
                            P::Variant {
                                def: *d,
                                variant: i as u32,
                                args: vec![],
                            },
                            s.ty,
                            span,
                        ),
                        variant: Some(i as u32),
                        member: None,
                        name,
                    })
                    .collect();
                (slots, false)
            }
            _ => self.member_slots(s, kind),
        }
    }

    /// Slots of a union / nullable / `typeof` scrutinee: `null` first, then the live members.
    fn member_slots(&mut self, s: &hir::Expr, kind: &ScrutKind) -> (Vec<Slot>, bool) {
        let live = self.narrowed_live(s);
        let inner = self.cx.ty.opt_payload(s.ty).unwrap_or(s.ty);
        let is_union = self.cx.union_def(inner).is_some();
        let mut out = vec![];
        let mut partial = false;
        for (k, (pat, member)) in self.member_patterns(s.ty, s.span).into_iter().enumerate() {
            let has_null = self.cx.ty.opt_payload(s.ty).is_some();
            let variant = match (member, is_union) {
                (Some(_), true) => Some((k - usize::from(has_null)) as u32),
                _ => None,
            };
            if variant.is_some_and(|v| live.as_ref().is_some_and(|l| !l.contains(&v))) {
                partial = true;
                continue;
            }
            let name = match (kind, member, variant) {
                (ScrutKind::TypeOf, None, _) => "\"object\"".to_string(),
                (_, None, _) => "null".to_string(),
                (ScrutKind::Discriminant(vals), _, Some(v)) => display_lit(&vals[v as usize]),
                (ScrutKind::TypeOf, Some(m), _) => format!("\"{}\"", self.cx.typeof_tag(m)),
                (_, Some(m), _) => match self.cx.lit_value(m) {
                    Some(v) => display_lit(&v),
                    None => format!("_: {}", self.cx.display(m)),
                },
            };
            out.push(Slot {
                pat,
                variant,
                member,
                name,
            });
        }
        (out, partial)
    }

    /// The variants a narrowed union local read can hold (`None`: not narrowed).
    fn narrowed_live(&self, s: &hir::Expr) -> Option<Vec<u32>> {
        match &s.kind {
            hir::ExprKind::Local(l, _) => self.allowed_members(*l),
            hir::ExprKind::UnwrapSome(base, _) => self.narrowed_live(base),
            _ => None,
        }
    }

    /// Is `e` a name that is not a value (a type / enum / module item used as `X.y`)?
    pub(crate) fn is_type_name(&self, e: &ast::Expr) -> bool {
        match &e.kind {
            ast::ExprKind::Ident(id) if self.is_namespace(&id.name) => true,
            ast::ExprKind::Ident(id) => {
                !self.is_local_name(&id.name)
                    && self
                        .cx
                        .lookup_item_at(self.module, &id.name, id.span)
                        .is_some_and(|it| match it {
                            Item::Def(d) => {
                                self.cx.global(d).is_none() && self.cx.try_fn(d).is_none()
                            }
                            Item::Alias(_) => true,
                        })
            }
            _ => false,
        }
    }
}

pub(super) fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip_parens(inner),
        _ => e,
    }
}

/// A short rendering of a `switch` discriminant / case value for messages.
pub(crate) fn source_text(e: &ast::Expr) -> String {
    match &strip_parens(e).kind {
        ast::ExprKind::Ident(id) => id.name.clone(),
        ast::ExprKind::This => "this".into(),
        ast::ExprKind::Member { object, prop, .. } => {
            format!("{}.{}", source_text(object), prop.name)
        }
        ast::ExprKind::Unary {
            op: ast::UnaryOp::TypeOf,
            expr,
        } => format!("typeof {}", source_text(expr)),
        ast::ExprKind::Lit(ast::Lit::Str(s)) => format!("{s:?}"),
        ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => value.to_string(),
        ast::ExprKind::Lit(ast::Lit::Bool(b)) => b.to_string(),
        _ => match literal_of(e) {
            Some(l) if l.negative => match l.lit {
                ast::Lit::Int { value, .. } => format!("-{value}"),
                ast::Lit::Float { value, .. } => format!("-{value}"),
                _ => "value".into(),
            },
            Some(ast::SignedLit {
                lit: ast::Lit::Float { value, .. },
                ..
            }) => value.to_string(),
            _ => "the value".into(),
        },
    }
}
