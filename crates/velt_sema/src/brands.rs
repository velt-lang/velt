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

    /// The object type that brands `t` (`{ __b?: true }` of `string & { __b?: true }`), if `t` is
    /// a brand.
    pub(crate) fn brand_tag(&self, t: TyId) -> Option<TyId> {
        let TyKind::Adt(d, args) = self.ty.kind(t) else {
            return None;
        };
        if !args.is_empty() || !self.brands.contains_key(d) {
            return None;
        }
        self.brand_keys
            .iter()
            .find(|(_, v)| *v == d)
            .map(|((_, tag), _)| *tag)
    }

    /// `t` with each brand whose object part mentions type parameters branded again by that
    /// part substituted with `args` (`symbol & { __t?: T }` at `T = number`).
    pub(crate) fn subst_brands(&mut self, t: TyId, args: &[Option<TyId>]) -> TyId {
        let mut found = vec![];
        self.ty.visit(t, &mut |k| {
            if let TyKind::Adt(d, a) = k {
                if a.is_empty() && !found.contains(d) {
                    found.push(*d);
                }
            }
        });
        found.retain(|d| self.generic_brands.contains(d));
        if found.is_empty() {
            return t;
        }
        let mut to = rustc_hash::FxHashMap::default();
        for d in found {
            let from = self.ty.intern(TyKind::Adt(d, vec![]));
            let (Some(base), Some(tag)) = (self.brand_base(from), self.brand_tag(from)) else {
                continue;
            };
            let tag = self.subst_known(tag, args);
            let module = self.adt(d).map_or(0, |a| a.module);
            let span = self.adt(d).map_or(Span::DUMMY, |a| a.span);
            to.insert(d, self.brand_type(base, tag, module, span));
        }
        self.ty.map(t, &mut |k| match k {
            TyKind::Adt(d, a) if a.is_empty() => to.get(d).copied(),
            _ => None,
        })
    }

    /// Can `t` be branded (a string, number, boolean or symbol type)?
    pub(crate) fn brandable(&self, t: TyId) -> bool {
        matches!(
            self.ty.kind(t),
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Symbol
        )
    }

    /// Can `t` be an intersection's branded part: a primitive, or a class instance
    /// (`Map<K, V> & { $set(v: Map<K, V>): void }`, the type of sigx's `signal(new Map())`)? A
    /// class instance brand is that instance, as `x as C & { … }` is in TypeScript: the class's
    /// members are its own, and the object part's members are read from the object at run time
    /// (`FnCx::brand_tag_member`). A `Record` is a dictionary, not such a part.
    pub(crate) fn brand_operand(&self, t: TyId) -> bool {
        if self.brandable(t) {
            return true;
        }
        match self.class_of(t) {
            Some((d, _)) => Some(d) != self.prelude_adt("Record") && !self.brands.contains_key(&d),
            None => false,
        }
    }

    /// Does the object part `tag` add nothing to `base`: no fields (`string & {}`, the
    /// literal-union idiom `T | (string & {})`, holds every string, as in TypeScript), or, for a
    /// class instance, only fields its class has with the same types (`C & { id: string }` is
    /// `C` when `C` declares `id: string`)?
    pub(crate) fn brand_adds_nothing(&mut self, base: TyId, tag: TyId) -> bool {
        if !self.is_object_type(tag) {
            return false;
        }
        let fields = self.public_fields(tag);
        if fields.is_empty() || self.class_of(base).is_none() {
            return fields.is_empty();
        }
        let have = self.public_fields(base);
        fields.iter().all(|f| {
            have.iter()
                .any(|h| h.name == f.name && h.ty == f.ty && (f.optional || !h.optional))
        })
    }

    /// Does every instance of the class `base` have the members of `tag` (`C & { y?: number }`,
    /// `C & { inc(): number }` where `C` has `inc(): number`)? Then a plain instance converts to
    /// the brand, as TypeScript accepts: each member is optional, or a field, method or getter
    /// of the class of the same type.
    pub(crate) fn instance_has_tag(&mut self, base: TyId, tag: TyId) -> bool {
        let Some((d, args)) = self.class_of(base) else {
            return false;
        };
        let have = self.public_fields(base);
        for f in self.public_fields(tag) {
            if let Some(h) = have.iter().find(|h| h.name == f.name) {
                if h.ty != f.ty || (h.optional && !f.optional) {
                    return false;
                }
                continue;
            }
            let member = match crate::collect::lookup_method(self, d, &args, &f.name) {
                Some(crate::collect::Found::Class { m, args, .. }) if !m.is_static => {
                    Some((m.def, args))
                }
                _ => None,
            };
            let Some((def, margs)) = member else {
                if f.optional {
                    continue;
                }
                return false;
            };
            let info = self.fn_info(def);
            let (getter, ret) = (info.is_getter, info.ret);
            let params: Vec<TyId> = info.params.iter().map(|p| p.ty).collect();
            let ret = self.subst(ret, &margs);
            let fits = match getter {
                true => ret == f.ty,
                false => match self.ty.kind(f.ty).clone() {
                    TyKind::FnPtr {
                        params: want,
                        ret: r,
                        ..
                    } => {
                        r == ret
                            && want.len() == params.len()
                            && (want.iter().zip(&params)).all(|(&w, &p)| self.subst(p, &margs) == w)
                    }
                    _ => false,
                },
            };
            if !fits {
                return false;
            }
        }
        true
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
            redefined: vec![],
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
        let mut params = vec![];
        crate::types::collect_params(&self.ty, tag, &mut params);
        if !params.is_empty() {
            self.generic_brands.insert(d);
        }
        self.ty.intern(TyKind::Adt(d, vec![]))
    }
}

impl crate::body::FnCx<'_, '_> {
    /// `h` as its primitive when it is branded (a use as the primitive: a member, an operand,
    /// a conversion); `h` itself otherwise. The value is the same, so only the type changes.
    pub(crate) fn unbrand(&mut self, mut h: hir::Expr) -> hir::Expr {
        if let Some(base) = self.cx.brand_base(h.ty) {
            h.ty = base;
        } else if let TyKind::Option(p, n) = *self.cx.ty.kind(h.ty) {
            // `B | null` of a brand `B` is its base's `| null`: the same value and layout.
            if let Some(base) = self.cx.brand_base(p) {
                h.ty = self.cx.ty.option_of(base, n);
            }
        }
        h
    }
}
