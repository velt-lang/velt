//! Branded types: `string & { __brand: "UserId" }`, a primitive intersected with object types
//! (design on #384). A brand is a nominal, zero-cost alias of its primitive: a value is branded
//! with `x as UserId`, a branded value is used wherever its primitive is (`id.length`,
//! `id + "!"`, `takesString(id)`), and a plain primitive does not convert to the brand. Two
//! brands of one primitive with the same object part are the same type, as in TypeScript.
//!
//! While bodies are checked a brand is its own type (a field-less definition, `Ctx::brands`);
//! a use as the primitive retypes the expression ([`FnCx::unbrand`]), and before lowering every
//! brand is replaced by its primitive (`crate::readonly::erase`), so nothing after sema sees it.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, Generics};
use crate::hir::{self, AdtKind, TyId, TyKind};

impl Ctx<'_> {
    /// The primitive of brand `t`, if `t` is a brand.
    pub(crate) fn brand_base(&self, t: TyId) -> Option<TyId> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if args.is_empty() => self.brands.get(d).copied(),
            _ => None,
        }
    }

    /// Can `t` be branded (a string, number or boolean type)?
    pub(crate) fn brandable(&self, t: TyId) -> bool {
        matches!(
            self.ty.kind(t),
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool
        )
    }

    /// The brand of primitive `base` by object type `tag` (one per pair).
    pub(crate) fn brand_type(&mut self, base: TyId, tag: TyId, module: usize, span: Span) -> TyId {
        if let Some(&d) = self.brand_keys.get(&(base, tag)) {
            return self.ty.intern(TyKind::Adt(d, vec![]));
        }
        let name = format!("{} & {}", self.display(base), self.display(tag));
        let info = AdtInfo {
            name: name.clone(),
            qual_name: name,
            kind: AdtKind::Struct,
            module,
            span,
            generics: Generics::default(),
            fields: vec![],
            own_fields_start: 0,
            base: None,
            methods: Default::default(),
            ctor: None,
            own_ctor: None,
            vtable: vec![],
            vslots: Default::default(),
            implements: vec![],
            has_dispose: false,
            statics: Default::default(),
            decl: None,
        };
        let d = self.alloc_def(span, DefInfo::Adt(Box::new(info)));
        self.brands.insert(d, base);
        self.brand_keys.insert((base, tag), d);
        self.ty.intern(TyKind::Adt(d, vec![]))
    }
}

impl crate::body::FnCx<'_, '_> {
    /// `h` as its primitive when it is branded (a use as the primitive: a member, an operand,
    /// a conversion); `h` itself otherwise. The value is the same, so only the type changes.
    pub(crate) fn unbrand(&mut self, mut h: hir::Expr) -> hir::Expr {
        if let Some(base) = self.cx.brand_base(h.ty) {
            h.ty = base;
        }
        h
    }
}
