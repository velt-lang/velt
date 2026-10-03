//! Monomorphic types: substitution of generic parameters, the HIR type → VIR type mapping, and
//! type properties lowering needs (drop glue required, display names).
//!
//! Representation (layouts in layout.rs):
//! - ints/floats/bool → scalars (`isize`/`usize` = `I64`/`U64`); `string` → `Agg(STR_AGG)`;
//! - literal types (`"circle"`) → nothing (zero-sized, like `void`);
//! - struct / anon / tuple → inline aggregate; class → `Ptr` to a heap object;
//! - numeric / string enum (no variant has a payload) → `I64` discriminant (string enums: the
//!   member index); unions → tagged aggregate (tag `u32` = variant index, then the payload);
//! - `T | null` → `Ptr` (null = none) when `T` is a class or `shared<T>`, else `{ bool, T }`;
//! - `Result<T, E>` → tagged aggregate (tag `u8`: 0 = Ok, 1 = Err);
//! - `T[]` → `{ ptr, len: u64, cap: u64 }`; `shared<T>` → `Ptr` to `{ count: u64, value: T }`;
//! - `Promise<T>` (a promise *value*: stored, passed, not awaited in place) → `Ptr` to a heap
//!   future (`VeltFut*`, result at +16; rt_abi_async.md), dropped with `velt_rt_fut_drop`;
//! - function values and closures → `{ code: ptr, env: ptr }`; interface values → `{ data, vtable }`.

use std::collections::HashMap;

use velt_sema::hir::{self, AdtKind, DefId, FloatTy, IntTy, TyId, TyKind};

use super::{ice, Cx};
use crate::vir::{Ty, STR_AGG};

impl<'h> Cx<'h> {
    pub(super) fn kind(&self, t: TyId) -> TyKind {
        self.types.kind(t).clone()
    }

    pub(super) fn intern(&mut self, k: TyKind) -> TyId {
        self.types.intern(k)
    }

    /// Replace `TyKind::Param(n)` by `targs[n]` throughout `t`, with anonymous object types in
    /// their canonical form (`canon`).
    pub(super) fn subst(&mut self, t: TyId, targs: &[TyId]) -> TyId {
        let t = self.subst_raw(t, targs);
        self.canon(t)
    }

    /// `t` with every anonymous object type replaced by the one type of its shape (field names
    /// and canonical field types, in order). Substitution makes several types of one shape:
    /// `{ a: U }` at `U = string` is the generic anonymous def applied to `[string]`, while a
    /// written `{ a: string }` is a def of its own. Sema canonicalizes the types it computes
    /// (velt_sema `anon.rs`), but instantiating a generic body happens here. The type of a shape
    /// is sema's concrete def of it when there is one, else the first one seen. The forms have
    /// the same layout; this keeps them one type, so values flow between them unconverted.
    pub(super) fn canon(&mut self, t: TyId) -> TyId {
        if let Some(&c) = self.anon.memo.get(&t) {
            return c;
        }
        let c = |cx: &mut Self, x: TyId| cx.canon(x);
        let k = match self.kind(t) {
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|&a| c(self, a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|&a| c(self, a)).collect()),
            TyKind::Array(e) => TyKind::Array(c(self, e)),
            TyKind::Map(k, v) => TyKind::Map(c(self, k), c(self, v)),
            TyKind::Tuple(es) => TyKind::Tuple(es.iter().map(|&a| c(self, a)).collect()),
            TyKind::Option(e) => TyKind::Option(c(self, e)),
            TyKind::Result(a, b) => TyKind::Result(c(self, a), c(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(c(self, v), c(self, e)),
            TyKind::Shared(e) => TyKind::Shared(c(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => TyKind::FnPtr {
                params: params.iter().map(|&a| c(self, a)).collect(),
                ret: c(self, ret),
                throws: c(self, throws),
            },
            _ => {
                self.anon.memo.insert(t, t);
                return t;
            }
        };
        let mut out = self.intern(k);
        if let TyKind::Adt(d, args) = self.kind(out) {
            let anon = match self.hir.def(d) {
                hir::Def::Adt(a) if a.kind == AdtKind::Anon && !args.is_empty() => Some(a),
                _ => None,
            };
            if let Some(a) = anon {
                let fields: Vec<(String, TyId)> =
                    a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
                let key: Vec<(String, TyId)> = fields
                    .into_iter()
                    .map(|(n, ft)| {
                        let ft = self.subst_raw(ft, &args);
                        (n, self.canon(ft))
                    })
                    .collect();
                out = match self.concrete_anon(&key) {
                    Some(dc) => self.intern(TyKind::Adt(dc, vec![])),
                    None => *self.anon.reps.entry(key).or_insert(out),
                };
            } else if let Some(u) = self.canon_union(d, &args) {
                out = u;
            }
        }
        self.anon.memo.insert(t, out);
        out
    }

    /// The canonical type of union def `d` at (canonical) `args` (velt_sema `anon.rs`
    /// `canon_union`): with plain members only, the deduplicated members' union (sema's def of
    /// that member list, else the first type seen with it), or the single member itself.
    fn canon_union(&mut self, d: DefId, args: &[TyId]) -> Option<TyId> {
        if args.is_empty() {
            return None;
        }
        let hir::Def::Enum(e) = self.hir.def(d) else {
            return None;
        };
        if !e.is_union {
            return None;
        }
        let payloads: Vec<TyId> = e.variants.iter().map(|v| v.payload[0]).collect();
        let mut members: Vec<TyId> = vec![];
        for p in payloads {
            let m = self.subst_raw(p, args);
            let m = self.canon(m);
            let plain = !matches!(
                self.kind(m),
                TyKind::Option(_) | TyKind::Unit | TyKind::Never | TyKind::Error
            ) && !self.is_union(m);
            if !plain {
                return None;
            }
            if !members.contains(&m) {
                members.push(m);
            }
        }
        if members.len() == 1 {
            return Some(members[0]);
        }
        members.sort_by_key(|t| t.0);
        let t = self.intern(TyKind::Adt(d, args.to_vec()));
        Some(match self.hir.union_shapes.get(&members) {
            Some(&u) => self.intern(TyKind::Adt(u, vec![])),
            None => *self.anon.union_reps.entry(members).or_insert(t),
        })
    }

    /// Where variant `variant` of the HIR union type `hir_ty` (of the current instance) lives in
    /// its canonical type `ty` (`canon_union`): another variant index, or the whole value when
    /// the union collapsed to that member. Unchanged for every other enum.
    pub(super) fn union_variant(
        &mut self,
        hir_ty: TyId,
        targs: &[TyId],
        variant: u32,
        ty: TyId,
    ) -> VariantAt {
        let raw = self.subst_raw(hir_ty, targs);
        if raw == ty {
            return VariantAt::Index(variant);
        }
        let TyKind::Adt(d, args) = self.kind(raw) else {
            return VariantAt::Index(variant);
        };
        let payload = self.enum_def(d).variants[variant as usize].payload[0];
        let m = self.subst_raw(payload, &args);
        let m = self.canon(m);
        if m == ty {
            return VariantAt::Whole;
        }
        let TyKind::Adt(cd, cargs) = self.kind(ty) else {
            ice("union variant of a non-union type")
        };
        let cps: Vec<TyId> = self
            .enum_def(cd)
            .variants
            .iter()
            .map(|v| v.payload[0])
            .collect();
        for (i, p) in cps.into_iter().enumerate() {
            let p = self.subst_raw(p, &cargs);
            if self.canon(p) == m {
                return VariantAt::Index(i as u32);
            }
        }
        ice("union variant not in its canonical union")
    }

    /// Sema's anonymous def with exactly these (concrete) fields, if lowering sees one.
    fn concrete_anon(&self, key: &[(String, TyId)]) -> Option<DefId> {
        self.hir.anon_shapes.get(key).copied()
    }

    /// [`subst`](Self::subst) without canonicalizing.
    fn subst_raw(&mut self, t: TyId, targs: &[TyId]) -> TyId {
        if targs.is_empty() {
            return t;
        }
        let s = |cx: &mut Self, x: TyId| cx.subst_raw(x, targs);
        let k = match self.kind(t) {
            TyKind::Param(n) => {
                return *targs
                    .get(n as usize)
                    .unwrap_or_else(|| ice(format_args!("type parameter #{n} has no argument")))
            }
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|&a| s(self, a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|&a| s(self, a)).collect()),
            TyKind::Array(e) => TyKind::Array(s(self, e)),
            TyKind::Map(k, v) => TyKind::Map(s(self, k), s(self, v)),
            TyKind::Tuple(es) => TyKind::Tuple(es.iter().map(|&a| s(self, a)).collect()),
            TyKind::Option(e) => TyKind::Option(s(self, e)),
            TyKind::Result(a, b) => TyKind::Result(s(self, a), s(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(s(self, v), s(self, e)),
            TyKind::Shared(e) => TyKind::Shared(s(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => TyKind::FnPtr {
                params: params.iter().map(|&a| s(self, a)).collect(),
                ret: s(self, ret),
                throws: s(self, throws),
            },
            _ => return t,
        };
        self.intern(k)
    }

    pub(super) fn adt_def(&self, d: DefId) -> &'h hir::AdtDef {
        match self.hir.def(d) {
            hir::Def::Adt(a) => a,
            _ => ice("expected a struct/class definition"),
        }
    }

    pub(super) fn enum_def(&self, d: DefId) -> &'h hir::EnumDef {
        match self.hir.def(d) {
            hir::Def::Enum(e) => e,
            _ => ice("expected an enum definition"),
        }
    }

    pub(super) fn fn_def(&self, d: DefId) -> &'h hir::FnDef {
        match self.hir.def(d) {
            hir::Def::Fn(f) => f,
            _ => ice("expected a function definition"),
        }
    }

    /// The `[Symbol.dispose]()` drop hook of a struct/class definition.
    pub(super) fn dispose_of(&self, d: DefId) -> Option<DefId> {
        match self.hir.def(d) {
            hir::Def::Adt(a) => a.dispose,
            _ => None,
        }
    }

    /// Is `t` a class instance type?
    pub(super) fn is_class(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) => {
                matches!(self.hir.def(*d), hir::Def::Adt(a) if a.kind == AdtKind::Class)
            }
            _ => false,
        }
    }

    /// Enum whose variants carry no payload: represented by its `i64` discriminant.
    pub(super) fn is_c_like_enum(&self, d: DefId) -> bool {
        self.enum_def(d)
            .variants
            .iter()
            .all(|v| v.payload.is_empty())
    }

    /// Is `t` a union type (a compiler-generated enum, `EnumDef::is_union`)?
    pub(super) fn is_union(&self, t: TyId) -> bool {
        match self.types.kind(t) {
            TyKind::Adt(d, _) => matches!(self.hir.def(*d), hir::Def::Enum(e) if e.is_union),
            _ => false,
        }
    }

    /// `Option<t>` uses the null pointer for none.
    pub(super) fn has_null_niche(&self, t: TyId) -> bool {
        self.is_class(t) || self.boxed(t) || matches!(self.types.kind(t), TyKind::Shared(_))
    }

    /// VIR type of a concrete HIR type. `Unit` and `Never` map to `Ty::Unit` (no value), and
    /// an option of such a type to `Bool`.
    pub(super) fn ty(&mut self, t: TyId) -> Ty {
        match self.kind(t) {
            TyKind::Int(i) => int_ty(i),
            TyKind::Float(FloatTy::F32) => Ty::F32,
            TyKind::Float(FloatTy::F64) => Ty::F64,
            TyKind::Bool => Ty::Bool,
            TyKind::Str => Ty::Agg(STR_AGG),
            // Literal types are zero-sized: the type is the value.
            TyKind::Unit | TyKind::Never | TyKind::Literal(_) => Ty::Unit,
            _ if self.boxed(t) => Ty::Ptr,
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Adt(a) if a.kind == AdtKind::Class => Ty::Ptr,
                hir::Def::Adt(_) => Ty::Agg(self.value_agg(t)),
                hir::Def::Enum(_) if self.is_c_like_enum(d) => Ty::I64,
                hir::Def::Enum(_) => Ty::Agg(self.value_agg(t)),
                _ => ice("type refers to a non-type definition"),
            },
            TyKind::Option(inner) if self.has_null_niche(inner) => Ty::Ptr,
            // No payload to store (`void`, `never`, a literal): the option is its present flag.
            TyKind::Option(inner) if self.ty(inner) == Ty::Unit => Ty::Bool,
            TyKind::Tuple(_) | TyKind::Option(_) | TyKind::Result(..) => Ty::Agg(self.value_agg(t)),
            TyKind::Array(_) => Ty::Agg(self.array_agg()),
            TyKind::Shared(_) | TyKind::Promise(..) => Ty::Ptr,
            TyKind::FnPtr { .. } | TyKind::Closure(_) => Ty::Agg(self.closure_agg()),
            TyKind::Dyn(..) => Ty::Agg(self.dyn_agg()),
            k => ice(format_args!("type {k:?} cannot be lowered")),
        }
    }

    /// Does a value of this type own resources that must be released (drop glue)?
    pub(super) fn needs_drop(&mut self, t: TyId) -> bool {
        if let Some(&b) = self.lay.drop_memo.get(&t) {
            return b;
        }
        // Provisional answer for recursive types (a type reached again through itself).
        self.lay.drop_memo.insert(t, false);
        let b = match self.kind(t) {
            _ if self.boxed(t) => true,
            TyKind::Str
            | TyKind::Array(_)
            | TyKind::Shared(_)
            | TyKind::Promise(..)
            | TyKind::FnPtr { .. }
            | TyKind::Dyn(..) => true,
            TyKind::Closure(d) => self.closure_env_is_heap(d),
            TyKind::Adt(..) if self.is_class(t) => true,
            TyKind::Adt(d, _) if self.dispose_of(d).is_some() => true,
            TyKind::Adt(..) | TyKind::Tuple(_) | TyKind::Result(..) => {
                self.part_types(t).into_iter().any(|p| self.needs_drop(p))
            }
            TyKind::Option(e) => self.needs_drop(e),
            _ => false,
        };
        self.lay.drop_memo.insert(t, b);
        b
    }

    /// Every component type stored inline in a value of `t` (fields, payloads, elements).
    pub(super) fn part_types(&mut self, t: TyId) -> Vec<TyId> {
        match self.kind(t) {
            TyKind::Adt(d, args) => match self.hir.def(d) {
                hir::Def::Adt(_) => self.adt_field_tys(t),
                hir::Def::Enum(e) => {
                    let tys: Vec<TyId> =
                        e.variants.iter().flat_map(|v| v.payload.clone()).collect();
                    tys.into_iter().map(|p| self.subst(p, &args)).collect()
                }
                _ => vec![],
            },
            TyKind::Tuple(es) => es,
            TyKind::Result(a, b) => vec![a, b],
            TyKind::Option(e) => vec![e],
            _ => vec![],
        }
    }

    /// Field types of a struct/class/anon instance, substituted with its type args.
    pub(super) fn adt_field_tys(&mut self, t: TyId) -> Vec<TyId> {
        let TyKind::Adt(d, args) = self.kind(t) else {
            ice("field types of a non-ADT type")
        };
        let tys: Vec<TyId> = self.adt_def(d).fields.iter().map(|f| f.ty).collect();
        tys.into_iter().map(|f| self.subst(f, &args)).collect()
    }

    /// Payload types of enum variant `v` of the concrete enum type `t`.
    pub(super) fn variant_tys(&mut self, t: TyId, v: u32) -> Vec<TyId> {
        match self.kind(t) {
            TyKind::Adt(d, args) => {
                let tys = self.enum_def(d).variants[v as usize].payload.clone();
                tys.into_iter().map(|p| self.subst(p, &args)).collect()
            }
            TyKind::Result(a, b) => vec![if v == 0 { a } else { b }],
            k => ice(format_args!("variant of non-enum type {k:?}")),
        }
    }

    pub(super) fn is_never(&self, t: TyId) -> bool {
        matches!(self.types.kind(t), TyKind::Never)
    }

    /// A concrete error type, `None` when nothing can be thrown: `Never`, or a union (built by
    /// substituting into a generic union) whose members are all empty.
    pub(super) fn error_ty(&mut self, t: Option<TyId>) -> Option<TyId> {
        let t = t?;
        if self.is_never(t) {
            return None;
        }
        if self.is_union(t) {
            let TyKind::Adt(d, _) = self.kind(t) else {
                return Some(t);
            };
            let n = self.enum_def(d).variants.len() as u32;
            let empty = (0..n).all(|v| {
                let p = self.variant_tys(t, v)[0];
                self.error_ty(Some(p)).is_none()
            });
            return (!empty).then_some(t);
        }
        Some(t)
    }

    /// What calling `f<targs>` (a sync function) throws, if anything.
    pub(super) fn fn_throws(&mut self, f: &hir::FnDef, targs: &[TyId]) -> Option<TyId> {
        let t = f.throws.map(|e| self.subst(e, targs));
        self.error_ty(t)
    }

    pub(super) fn is_unit(&mut self, t: TyId) -> bool {
        self.ty(t) == Ty::Unit
    }

    /// Short display name of a type (`Uncaught <Type>`, `Name { ... }` when printing).
    pub(super) fn type_name(&self, t: TyId) -> String {
        let full = match self.types.kind(t) {
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) => match self.hir.def(*d) {
                hir::Def::Adt(a) => a.name.clone(),
                hir::Def::Enum(e) => e.name.clone(),
                hir::Def::Interface(i) => i.name.clone(),
                _ => "?".into(),
            },
            TyKind::Str => "string".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Int(_) | TyKind::Float(_) => "number".into(),
            // Spelled without intern indices: aggregate names must not change with unrelated
            // edits either (`velt dev` compares layouts across versions by name).
            _ => return self.type_key(t),
        };
        match full.rfind([':', '.', '/']) {
            Some(i) => full[i + 1..].to_string(),
            None => full,
        }
    }

    pub(super) fn str_ty(&mut self) -> TyId {
        self.intern(TyKind::Str)
    }
}

pub(super) fn int_ty(i: IntTy) -> Ty {
    match i {
        IntTy::I8 => Ty::I8,
        IntTy::I16 => Ty::I16,
        IntTy::I32 => Ty::I32,
        IntTy::I64 | IntTy::ISize => Ty::I64,
        IntTy::U8 => Ty::U8,
        IntTy::U16 => Ty::U16,
        IntTy::U32 => Ty::U32,
        IntTy::U64 | IntTy::USize => Ty::U64,
    }
}

/// Canonical anonymous object types (`Cx::canon`).
#[derive(Default)]
pub(super) struct AnonShapes {
    memo: HashMap<TyId, TyId>,
    /// The type of each shape that sema has no concrete def for.
    reps: HashMap<Vec<(String, TyId)>, TyId>,
    /// The same for unions, by sorted member list.
    union_reps: HashMap<Vec<TyId>, TyId>,
}

/// Where a generic union's variant lives in the canonical union (`Cx::union_variant`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VariantAt {
    Index(u32),
    /// The union collapsed to this member: the value itself.
    Whole,
}
