//! Anonymous object types (`{ a: 1 }` literals, `{ kind: "circle"; readonly r: f64 }` written
//! types): one synthesized `AdtKind::Anon` def per shape (field names, declared types, `readonly`
//! and optional flags, in order), generic over the type parameters its fields mention (renumbered
//! by first occurrence). Shapes that differ only in `readonly` convert to each other
//! ([`Ctx::same_layout`]). An optional field (`a?: T`) and a `a: T | null` one hold the same
//! values, but are different shapes, as in TypeScript: `JSON.stringify` leaves out the first
//! while it is absent (JavaScript's missing property) and writes the second.
//!
//! Substitution keeps types canonical: `{ a: U }` is the def `{ a: T0 }` applied to `[U]`, and
//! with `U = string` it must be the same type as a written `{ a: string }` (the def
//! `{ a: string }`, no arguments). [`Ctx::subst`] substitutes and then re-interns every
//! anonymous object type from its substituted fields ([`Ctx::canon`]). Both forms have the same
//! fields in the same order, so they lay out alike (`velt_vir` maps both to one type), and
//! `assigned_fields.rs` makes them agree on `AdtDef::assigned`.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, FieldInfo, Generics};
use crate::hir::{AdtKind, DefId, TyId, TyKind};

/// One field of an anonymous object type's shape.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShapeField {
    pub name: String,
    /// The declared type, without the `null` a `?` adds (`FieldInfo::declared`).
    pub ty: TyId,
    pub readonly: bool,
    /// Declared `name?: T`: may be absent, and reads as `T | null`.
    pub optional: bool,
}

impl ShapeField {
    /// A field neither readonly nor optional.
    pub fn plain(name: &str, ty: TyId) -> ShapeField {
        ShapeField {
            name: name.to_string(),
            ty,
            readonly: false,
            optional: false,
        }
    }
}

/// What the `Object` built-ins (`body::expr::object_builtins`) make, program-wide.
#[derive(Default)]
pub(crate) struct ObjectState {
    /// The type of `Object.freeze(o)` for each anonymous object type `o` frozen: a readonly def
    /// of its own, which doesn't convert to a type whose fields can be written (`frozen_defs`).
    pub frozen: FxHashMap<DefId, DefId>,
    /// The values of `frozen`.
    pub frozen_defs: FxHashSet<DefId>,
    /// The program calls `Object.create(null)`: records may have no prototype.
    pub null_proto_records: bool,
    /// `Object.defineProperty` brands (`crate::defined_props`).
    pub brands: crate::defined_props::Brands,
    /// The key names of the brands' hidden fields, once resolved (`Ctx::hidden_keys`).
    pub hidden: Option<FxHashSet<String>>,
    /// Each class's value per construct signature type, a module-state constant
    /// (`body::expr::ctor_values`), in the order they were made.
    pub class_values: Vec<((DefId, TyId), DefId)>,
}

impl Ctx<'_> {
    /// Replace `Param(i)` by `args[i]` in `t`, keeping anonymous object types canonical.
    pub fn subst(&mut self, t: TyId, args: &[TyId]) -> TyId {
        let s = self.ty.subst(t, args);
        if s != t && !self.type_level.op_sites.is_empty() {
            self.carry_op_sites(t, &mut |cx, o| cx.ty.subst(o, args));
        }
        self.canon(s)
    }

    /// [`Types::subst_known`](crate::types::Types::subst_known), keeping anonymous object
    /// types canonical.
    pub fn subst_known(&mut self, t: TyId, slots: &[Option<TyId>]) -> TyId {
        let s = self.ty.subst_known(t, slots);
        if s != t && !self.type_level.op_sites.is_empty() {
            self.carry_op_sites(t, &mut |cx, o| cx.ty.subst_known(o, slots));
        }
        self.canon(s)
    }

    /// Gives each deferred type in `t`, as `sub` substitutes it, the site and module of the one
    /// written (`TypeLevel::op_sites`), so a reduction reports where the type was written.
    pub(crate) fn carry_op_sites(&mut self, t: TyId, sub: &mut dyn FnMut(&mut Self, TyId) -> TyId) {
        let mut stack = vec![t];
        let mut seen = rustc_hash::FxHashSet::default();
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            let k = self.ty.kind(x).clone();
            if let TyKind::Op(..) = k {
                if let Some(&site) = self.type_level.op_sites.get(&x) {
                    let to = sub(self, x);
                    self.type_level.op_sites.entry(to).or_insert(site);
                    if let Some(&m) = self.type_level.op_modules.get(&x) {
                        self.type_level.op_modules.entry(to).or_insert(m);
                    }
                }
            }
            stack.extend(crate::types::children(&k));
        }
    }

    /// `t` with every anonymous object type in its canonical form: the def of its substituted
    /// field list, as if written directly.
    pub fn canon(&mut self, t: TyId) -> TyId {
        self.canon_depth(t, 0)
    }

    fn canon_depth(&mut self, t: TyId, depth: u32) -> TyId {
        if let Some(&c) = self.canon_memo.get(&t) {
            return c;
        }
        // Instances of object types that reach themselves are left as they are
        // (`reaches_itself`), so this only guards against an ICE elsewhere. Should it fire, the
        // callers memoize a form that is canonical only above this depth; nothing reaches it.
        if depth > 64 {
            return t;
        }
        let c = |cx: &mut Self, x: TyId| cx.canon_depth(x, depth + 1);
        let k = match self.ty.kind(t).clone() {
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|a| c(self, *a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|a| c(self, *a)).collect()),
            TyKind::Op(crate::hir::TyOp::ErrSite(_), _) => return t,
            TyKind::Op(o, args) => {
                // A deferred type whose arguments are all known now is evaluated (#934).
                let args: Vec<TyId> = args.iter().map(|a| c(self, *a)).collect();
                let op = self.ty.intern(TyKind::Op(o, args));
                let mut ps = vec![];
                crate::types::collect_params(&self.ty, op, &mut ps);
                // An intersection whose parts are all known object types is reduced even when
                // their fields still mention type parameters: `Signal<T>` at `T = { data: D }`
                // in a generic function is `{ data: D; $set(v: { data: D }): void }`.
                if op != t {
                    if let Some(&s) = self.type_level.op_sites.get(&t) {
                        self.type_level.op_sites.entry(op).or_insert(s);
                    }
                    if let Some(&m) = self.type_level.op_modules.get(&t) {
                        self.type_level.op_modules.entry(op).or_insert(m);
                    }
                }
                let out = if ps.is_empty() || self.reducible_now(op) {
                    self.reduce_op(op)
                } else {
                    op
                };
                self.canon_memo.insert(t, out);
                return out;
            }
            TyKind::Array(e) => TyKind::Array(c(self, e)),
            TyKind::Map(a, b) => TyKind::Map(c(self, a), c(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| c(self, *a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(c(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(c(self, a), c(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(c(self, v), c(self, e)),
            TyKind::Shared(e) => TyKind::Shared(c(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|a| c(self, *a)).collect(),
                ret: c(self, ret),
                throws: c(self, throws),
                rest,
            },
            _ => {
                self.canon_memo.insert(t, t);
                return t;
            }
        };
        let mut out = self.ty.intern(k);
        out = self.ty.inferred_like(t, out);
        if let TyKind::Adt(d, args) = self.ty.kind(out).clone() {
            if let Some(r) = self.canon_anon(d, &args, depth) {
                out = r;
            } else if let Some(r) = self.canon_union(d, &args, depth) {
                out = r;
            }
        }
        self.canon_memo.insert(t, out);
        out
    }

    /// The canonical type of union def `d` applied to (canonical) `args`: the union of the
    /// substituted members, as if written directly (`A | B` at `A = B = string` is `string`, at
    /// `[i64, string]` the written `string | i64`). Only when every member is a plain type: a
    /// member that is itself a union or nullable would flatten into its parts, and lowering maps
    /// one generic variant onto one canonical variant (velt_vir `Cx::canon`, `union_variant`).
    fn canon_union(&mut self, d: DefId, args: &[TyId], depth: u32) -> Option<TyId> {
        if args.is_empty() || args.iter().any(|a| self.ty.has_error(*a)) {
            return None;
        }
        let t = self.ty.intern(TyKind::Adt(d, args.to_vec()));
        self.union_def(t)?;
        let members: Vec<TyId> = self
            .union_members(t)?
            .into_iter()
            .map(|m| self.canon_depth(m, depth + 1))
            .collect();
        let plain = |cx: &Self, m: TyId| {
            !matches!(
                cx.ty.kind(m),
                TyKind::Option(..) | TyKind::Unit | TyKind::Never | TyKind::Error
            ) && cx.union_def(m).is_none()
        };
        // A dynamic member (`T | Mod<T>` at `T = unknown`) keeps the union: written, it would
        // be that `unknown`, but lowering maps each generic variant onto one of the instance.
        if !members
            .iter()
            .all(|m| plain(self, *m) && self.dynamic(*m).is_none())
        {
            return None;
        }
        let u = self.union_of(&members, false, velt_common::Span::DUMMY);
        (u != t).then_some(u)
    }

    /// The canonical type of anonymous def `d` applied to (canonical) `args`, if `d` is an
    /// anonymous def and that is a different type.
    fn canon_anon(&mut self, d: DefId, args: &[TyId], depth: u32) -> Option<TyId> {
        // `Error` arguments (unknown slots of an expected type) stay as they are: a def over
        // error fields would only be noise, and errors match anything anyway.
        if args.is_empty() || args.iter().any(|a| self.ty.has_error(*a)) {
            return None;
        }
        let a = self.adt(d).filter(|a| a.kind == AdtKind::Anon)?;
        let module = a.module;
        if self.reaches_itself(d) {
            return None;
        }
        let templ: Vec<ShapeField> = a.fields.iter().map(shape_field).collect();
        // The flags are part of the shape: `{ readonly v: T0 }` at `string` is
        // `{ readonly v: string }`, not the writable `{ v: string }`.
        let fields: Vec<ShapeField> = templ
            .into_iter()
            .map(|f| {
                let ty = self.ty.subst(f.ty, args);
                let ty = self.canon_depth(ty, depth + 1);
                ShapeField { ty, ..f }
            })
            .collect();
        let (d2, args2) = self.anon_def_with(&fields, module);
        if d2 == d && args2 == args {
            return None;
        }
        Some(self.ty.intern(TyKind::Adt(d2, args2)))
    }

    /// Does anonymous def `d` reach itself through the fields of object types
    /// (`interface List<T> { tail?: List<T> }`, #376)? Such an instance stays as it is:
    /// canonicalizing it would canonicalize its own fields without end, and no written object
    /// type spells its shape without naming it.
    pub(crate) fn reaches_itself(&self, d: DefId) -> bool {
        let object = |x: DefId| {
            self.adt(x)
                .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(&x))
        };
        let mut seen = rustc_hash::FxHashSet::default();
        let mut stack = self.anon_field_tys(d);
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            let k = self.ty.kind(t);
            if let TyKind::Adt(x, _) = k {
                if *x == d {
                    return true;
                }
                if object(*x) {
                    stack.extend(self.anon_field_tys(*x));
                }
            }
            stack.extend(crate::types::children(k));
        }
        false
    }

    /// The anonymous object type with these fields (none readonly or optional).
    pub fn anon_type(&mut self, fields: &[(String, TyId)], module: usize) -> TyId {
        let (d, args) = self.anon_def(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
    }

    /// The anonymous object type with these fields (declared types and flags).
    pub fn anon_type_with(&mut self, fields: &[ShapeField], module: usize) -> TyId {
        let (d, args) = self.anon_def_with(fields, module);
        self.ty.intern(TyKind::Adt(d, args))
    }

    /// Whether `a` and `b` are object types (anonymous ones, or field-only interfaces') that are
    /// one type for lowering (`crate::readonly`): the same fields, differing at most in
    /// `readonly` or in being a field-only interface. A value converts from one to the other and
    /// stays the same object. Equal fields alone are not enough: a generic type's instance
    /// (`Box<number>`) and the object type it spells out (`{ v: number }`) are different
    /// definitions after erasure, so converting between them would be a type mismatch in
    /// lowering.
    pub fn same_layout(&mut self, a: TyId, b: TyId) -> bool {
        let object = |cx: &Self, t: TyId| match cx.ty.kind(t) {
            TyKind::Adt(d, _) => cx
                .adt(*d)
                .is_some_and(|x| x.kind == AdtKind::Anon || cx.field_only_of.contains_key(d)),
            _ => false,
        };
        if a == b || !object(self, a) || !object(self, b) || self.readonly_twins.is_empty() {
            return false;
        }
        let twins = self.readonly_twins.clone();
        let mut cache = FxHashMap::default();
        // Erasure turns a generic field-only interface's instance (`Pair<string, number>`) into
        // its generic anonymous twin applied to the arguments; canonical, that is the object type
        // it spells out.
        let ea = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, a);
        let eb = crate::readonly::erase_ty(&mut self.ty, &twins, &mut cache, b);
        self.canon(ea) == self.canon(eb)
    }

    /// Are `a` and `b` anonymous defs with the same field names, in order?
    pub(crate) fn same_anon_shape(&self, a: DefId, b: DefId) -> bool {
        let names = |d: DefId| {
            self.adt(d)
                .filter(|x| x.kind == AdtKind::Anon)
                .map(|x| x.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>())
        };
        a != b && names(a).is_some_and(|n| Some(n) == names(b))
    }

    /// Field types of anonymous def `d`, over its own parameters.
    pub(crate) fn anon_field_tys(&self, d: DefId) -> Vec<TyId> {
        self.adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default()
    }

    /// Record where the fields of anonymous object type `t` are written (for editors), if no
    /// written object type of this shape was seen before.
    pub fn declare_anon_fields(&mut self, t: TyId, spans: &[Span]) {
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return;
        };
        let a = self.adt_mut(d);
        if a.fields.iter().all(|f| f.span == Span::DUMMY) && a.fields.len() == spans.len() {
            for (f, sp) in a.fields.iter_mut().zip(spans) {
                f.span = *sp;
            }
        }
    }

    /// The anonymous object def of this shape (no field readonly or optional) and its type
    /// arguments.
    pub fn anon_def(&mut self, fields: &[(String, TyId)], module: usize) -> (DefId, Vec<TyId>) {
        let fields: Vec<ShapeField> = fields
            .iter()
            .map(|(n, t)| ShapeField::plain(n, *t))
            .collect();
        self.anon_def_with(&fields, module)
    }

    /// The anonymous object def of this shape (declared types and flags) and its type
    /// arguments.
    pub fn anon_def_with(&mut self, fields: &[ShapeField], module: usize) -> (DefId, Vec<TyId>) {
        let mut params: Vec<u32> = vec![];
        for f in fields {
            crate::types::collect_params(&self.ty, f.ty, &mut params);
        }
        let new_tys: FxHashMap<u32, TyId> = params
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.ty.param(i as u32)))
            .collect();
        let norm: Vec<ShapeField> = fields
            .iter()
            .map(|f| {
                let ty = self.ty.map(f.ty, &mut |k| match k {
                    TyKind::Param(i) => new_tys.get(i).copied(),
                    _ => None,
                });
                ShapeField { ty, ..f.clone() }
            })
            .collect();
        let params: Vec<TyId> = params.iter().map(|p| self.ty.param(*p)).collect();
        if let Some(&d) = self.anon.get(&norm) {
            return (d, params);
        }
        let d = self.new_anon_def(&norm, params.len(), module);
        self.anon.insert(norm.clone(), d);
        if norm.iter().any(|f| f.readonly) {
            let plain: Vec<ShapeField> = norm
                .iter()
                .map(|f| ShapeField {
                    readonly: false,
                    ..f.clone()
                })
                .collect();
            let (twin, _) = self.anon_def_with(&plain, module);
            self.readonly_twins.insert(d, (twin, None));
        }
        (d, params)
    }

    /// The type of `Object.freeze(o)` for `o` of concrete anonymous object type `t`: its fields
    /// all readonly, in a def of its own (`Ctx::frozen`; one per frozen shape) that lowering
    /// sees as `t`'s. `None`: `t` is not such a type.
    pub(crate) fn frozen_type(&mut self, t: TyId, module: usize) -> Option<TyId> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        if a.kind != AdtKind::Anon || !args.is_empty() || a.generics.len() != 0 {
            return None;
        }
        if let Some(&f) = self.objects.frozen.get(&d) {
            return Some(self.ty.intern(TyKind::Adt(f, vec![])));
        }
        let fields: Vec<ShapeField> = a
            .fields
            .iter()
            .map(|f| ShapeField {
                readonly: true,
                ..shape_field(f)
            })
            .collect();
        let plain: Vec<ShapeField> = fields
            .iter()
            .map(|f| ShapeField {
                readonly: false,
                ..f.clone()
            })
            .collect();
        let f = self.new_anon_def(&fields, 0, module);
        let (twin, _) = self.anon_def_with(&plain, module);
        self.readonly_twins.insert(f, (twin, None));
        self.objects.frozen.insert(d, f);
        self.objects.frozen_defs.insert(f);
        Some(self.ty.intern(TyKind::Adt(f, vec![])))
    }

    /// Would converting a value of type `from` to `to` let a field of an `Object.freeze`
    /// result be written (`to`, or a member of it, an object type with a field that is not
    /// readonly)?
    pub(crate) fn unfreezes(&mut self, from: TyId, to: TyId) -> bool {
        if self.objects.frozen_defs.is_empty() || from == to {
            return false;
        }
        let from = self.ty.opt_payload(from).unwrap_or(from);
        if !matches!(self.ty.kind(from), TyKind::Adt(d, _) if self.objects.frozen_defs.contains(d))
        {
            return false;
        }
        let to = self.ty.opt_payload(to).unwrap_or(to);
        let members = self.union_members(to).unwrap_or_else(|| vec![to]);
        members.iter().any(|m| match self.ty.kind(*m) {
            TyKind::Adt(d, _) => self
                .adt(*d)
                .is_some_and(|a| a.fields.iter().any(|f| !f.readonly)),
            TyKind::Dyn(..) => true,
            _ => false,
        })
    }

    /// A struct no source names, with fields `fields` (types over the type params `T0..Tn`), for
    /// a value sema builds itself: an object literal implementing an interface
    /// (`body/expr/iface_object.rs`). Unlike [`Self::anon_def`]'s it is never shared with
    /// another shape, so an implementation added for it belongs to it alone.
    pub(crate) fn hidden_struct_def(
        &mut self,
        name: &str,
        fields: &[(String, TyId)],
        n: usize,
        module: usize,
    ) -> DefId {
        let norm: Vec<ShapeField> = fields
            .iter()
            .map(|(f, t)| ShapeField::plain(f, *t))
            .collect();
        let d = self.new_anon_def(&norm, n, module);
        let a = self.adt_mut(d);
        a.kind = AdtKind::Struct;
        a.name = name.to_string();
        a.qual_name = name.to_string();
        d
    }

    /// A fresh anonymous object def with fields `norm` (over `n` type params).
    fn new_anon_def(&mut self, norm: &[ShapeField], n: usize, module: usize) -> DefId {
        let name = {
            let parts: Vec<String> = norm
                .iter()
                .map(|f| {
                    let readonly = if f.readonly { "readonly " } else { "" };
                    let q = if f.optional { "?" } else { "" };
                    format!("{readonly}{}{q}: {}", f.name, self.display(f.ty))
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        };
        let mut generics = Generics::default();
        for i in 0..n {
            generics.push(&format!("T{i}"));
        }
        let info = AdtInfo {
            name: name.clone(),
            qual_name: name,
            kind: AdtKind::Anon,
            module,
            span: Span::DUMMY,
            generics,
            fields: vec![],
            own_fields_start: 0,
            redefined: vec![],
            base: None,
            methods: FxHashMap::default(),
            ctor: None,
            own_ctor: None,
            vtable: vec![],
            vslots: FxHashMap::default(),
            implements: vec![],
            has_dispose: false,
            statics: FxHashMap::default(),
            decl: None,
        };
        let d = self.alloc_def(Span::DUMMY, DefInfo::Adt(Box::new(info)));
        let fields = norm
            .iter()
            .map(|f| {
                // An optional field reads as `T | undefined` and may be left out, like a class's
                // (`body::driver`): its default is `undefined`, which lowering's JSON and
                // printing glue reads as "absent".
                // `q?: any` holds the dynamic `undefined` itself (`collect::shapes`).
                let dynamic = f.optional && self.dynamic(f.ty).is_some();
                let ty = if f.optional && !dynamic {
                    self.ty.option_of(f.ty, crate::hir::Nothing::Undefined)
                } else {
                    f.ty
                };
                let default = if dynamic {
                    self.dynamic_undefined(ty, Span::DUMMY)
                } else {
                    f.optional.then_some(crate::hir::Expr {
                        kind: crate::hir::ExprKind::Lit(crate::hir::Lit::Null),
                        ty,
                        span: Span::DUMMY,
                    })
                };
                FieldInfo {
                    name: f.name.clone(),
                    ty,
                    declared: f.ty,
                    span: Span::DUMMY,
                    readonly: f.readonly,
                    optional: f.optional,
                    has_default: f.optional,
                    default,
                    default_throws: vec![],
                    private_to: None,
                }
            })
            .collect();
        self.adt_mut(d).fields = fields;
        d
    }
}

/// `hir::Program::anon_shapes`: every concrete anonymous def lowering sees, by its (erased)
/// fields. Defs replaced by a twin before lowering (`crate::readonly`) are left out: lowering
/// never sees them, and a shape must not resolve to one of them.
pub(crate) fn concrete_shapes(cx: &Ctx) -> FxHashMap<Vec<(String, TyId, bool, bool)>, DefId> {
    let mut out = FxHashMap::default();
    for (i, d) in cx.defs.iter().enumerate() {
        let id = DefId(i as u32);
        if let Some(crate::hir::Def::Adt(a)) = d {
            if a.kind == AdtKind::Anon && a.generics == 0 && !cx.readonly_twins.contains_key(&id) {
                let key = a
                    .fields
                    .iter()
                    .map(|f| {
                        // `{ d?: string }` and `{ d?: string | null }` read alike (one `null`),
                        // but only the second keeps a presence flag: two layouts.
                        (f.name.clone(), f.ty, f.optional, f.presence)
                    })
                    .collect();
                out.entry(key).or_insert(id);
            }
        }
    }
    out
}

/// `hir::Program::union_shapes`: every concrete union def by its member list (sorted by type id,
/// as `Ctx::union_of` orders concrete members).
pub(crate) fn concrete_unions(cx: &Ctx) -> FxHashMap<Vec<TyId>, DefId> {
    let mut out = FxHashMap::default();
    for (key, d) in &cx.unions {
        if let Some(crate::hir::Def::Enum(e)) = &cx.defs[d.0 as usize] {
            if e.generics == 0 {
                out.entry(key.clone()).or_insert(*d);
            }
        }
    }
    out
}

/// Field `f` as an anonymous object type's shape holds it.
pub(crate) fn shape_field(f: &FieldInfo) -> ShapeField {
    ShapeField {
        name: f.name.clone(),
        ty: f.declared,
        readonly: f.readonly,
        optional: f.optional,
    }
}

/// Whether field `f` of a type of kind `kind` keeps "absent" apart from a present value
/// (`hir::FieldDef::presence`): an optional field (`a?: T`, read as `T | undefined`) of an
/// object type or struct, so that an absent key and one holding `undefined` stay apart, as in
/// JavaScript (`"a" in o`, printing, spread; issue #930). That includes `a?: any` (and
/// `unknown`), whose absent value is the dynamic `undefined` with the flag clear (#1319). A
/// class's optional field is an own property holding `undefined` until set.
pub(crate) fn has_presence(ty: &crate::types::Types, kind: AdtKind, f: &FieldInfo) -> bool {
    f.optional
        && kind != AdtKind::Class
        && (ty.nothing_of(f.ty).is_some() || matches!(ty.kind(f.ty), crate::hir::TyKind::Dyn(..)))
}

/// Whether field `f` of a type of kind `kind` is optional without a presence flag (`a?: T`): an
/// `undefined` in it stands for "absent", so it cannot hold a present `undefined`
/// (`crate::absent_fields`).
pub(crate) fn absent_is_null(ty: &crate::types::Types, kind: AdtKind, f: &FieldInfo) -> bool {
    f.optional && kind != AdtKind::Class && !has_presence(ty, kind, f)
}
