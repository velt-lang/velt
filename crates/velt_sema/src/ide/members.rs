//! Members of types, for completion after `x.` / `Type.`: collected once from the checked
//! program (instance members of every struct / class / interface / object type in its own
//! generic space — inherited ones substituted —, static members, `extend` blocks) and displayed
//! for a concrete receiver type at query time.

use std::collections::HashMap;

use velt_common::{FileId, Span};

use super::defref::{Builder, DefRef};
use super::display::named_param;
use super::record::Target;
use super::Analysis;
use crate::collect::lookup_method;
use crate::ctx::Ctx;
use crate::defs::{is_setter_key, DefInfo, FnInfo};
use crate::hir::{AdtKind, DefId, TyId, TyKind, TyTable};

/// How a member is shown: a value of a type, or a method signature.
#[derive(Clone)]
pub(super) enum Shape {
    Value(TyId),
    Method(Vec<(String, TyId)>, TyId),
}

/// A member while collecting (definition not yet built).
struct Raw {
    name: String,
    target: Target,
    shape: Shape,
    /// Names of the member's own generic params, which follow the owner's.
    own: Vec<String>,
    private_to: Option<DefId>,
    /// A field declared optional (`name?: T`).
    optional: bool,
    /// The type that declares a method (a base class, an interface with a default, or the
    /// type itself).
    declared_in: Option<DefId>,
}

pub(super) struct Member {
    pub name: String,
    pub def: DefRef,
    pub shape: Shape,
    own: Vec<String>,
    private_to: Option<DefId>,
    pub optional: bool,
    pub declared_in: Option<DefId>,
}

/// What kind of type definition a [`TypeMembers`] lists, for the type query.
#[derive(Clone, Debug)]
pub(super) enum TypeKind {
    Struct,
    Class,
    /// An object type (`{ x: i64 }`).
    Anon,
    Interface,
    Enum,
    /// A union type (`string | i64`): the payload of each variant.
    Union(Vec<TyId>),
}

pub(super) struct TypeMembers {
    pub instance: Vec<Member>,
    statics: Vec<Member>,
    pub kind: TypeKind,
    /// Declared in the standard library.
    pub is_std: bool,
}

/// An `extend` block: target pattern over `n` params, its instance methods.
struct Extension {
    target: TyId,
    n: usize,
    methods: Vec<Member>,
}

pub(super) struct Members {
    pub types: HashMap<DefId, TypeMembers>,
    /// Declaration span of each type definition (for `DefRef` → `DefId`).
    by_span: HashMap<Span, DefId>,
    extensions: Vec<Extension>,
}

/// A type definition's raw members: instance, static, and what it is.
type RawType = (DefId, Vec<Raw>, Vec<Raw>, TypeKind, bool);

/// Collected raw tables (phase 1, needs `&mut Ctx` for substitution).
pub(super) struct RawMembers {
    types: Vec<RawType>,
    extensions: Vec<(TyId, usize, Vec<Raw>)>,
}

pub(super) fn collect(cx: &mut Ctx) -> RawMembers {
    let std_files: Vec<FileId> = cx
        .modules
        .iter()
        .filter(|m| m.is_std)
        .map(|m| m.file)
        .collect();
    let in_std = |m: usize| cx.modules.get(m).is_some_and(|m| m.is_std);
    let mut types = vec![];
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        let (kind, is_std) = match &cx.info[i] {
            DefInfo::Adt(a) => {
                let kind = match a.kind {
                    AdtKind::Anon => TypeKind::Anon,
                    AdtKind::Struct => TypeKind::Struct,
                    AdtKind::Class => TypeKind::Class,
                };
                (kind, in_std(a.module))
            }
            DefInfo::Iface(x) => (TypeKind::Interface, in_std(x.module)),
            DefInfo::Enum(e) if e.is_union => {
                let payloads = e.variants.iter().filter_map(|v| v.payload.first());
                (TypeKind::Union(payloads.copied().collect()), false)
            }
            DefInfo::Enum(e) => (TypeKind::Enum, std_files.contains(&e.span.file)),
            DefInfo::Fn(_) | DefInfo::Global(_) => continue,
        };
        let (instance, statics) = match &cx.info[i] {
            DefInfo::Adt(a) if a.decl.is_some() => (adt_instance(cx, d), adt_statics(cx, d)),
            // Object types (`{ href?: string }`) and field-only interfaces have fields only.
            DefInfo::Adt(_) => (adt_instance(cx, d), vec![]),
            DefInfo::Iface(x) if x.decl.is_some() => (iface_members(cx, d), vec![]),
            DefInfo::Enum(e) if e.decl.is_some() => {
                let vs = (0..e.variants.len())
                    .map(|k| Raw {
                        name: e.variants[k].name.clone(),
                        target: Target::Variant(d, k as u32),
                        shape: Shape::Value(cx.ty.error),
                        own: vec![],
                        private_to: None,
                        optional: false,
                        declared_in: None,
                    })
                    .collect();
                (vec![], vs)
            }
            _ => (vec![], vec![]),
        };
        types.push((d, instance, statics, kind, is_std));
    }
    let mut extensions = vec![];
    for e in 0..cx.extensions.len() {
        let x = &cx.extensions[e];
        let (target, n) = (x.target, x.generics.len());
        let mut ms: Vec<_> = x.methods.iter().map(|(n, m)| (n.clone(), *m)).collect();
        ms.sort_by(|a, b| a.0.cmp(&b.0));
        let raws = ms
            .into_iter()
            .filter(|(n, m)| !m.is_static && !is_setter_key(n))
            .map(|(n, m)| method_raw(cx, n, m.def, &[]))
            .collect();
        extensions.push((target, n, raws));
    }
    RawMembers { types, extensions }
}

fn method_raw(cx: &mut Ctx, name: String, def: DefId, owner_args: &[TyId]) -> Raw {
    let f = cx.fn_info(def).clone();
    let n_owner = owner_args.len();
    let subst = |cx: &mut Ctx, t: TyId| {
        if owner_args.is_empty() {
            t
        } else {
            cx.subst(t, owner_args)
        }
    };
    let ps = f
        .params
        .iter()
        .map(|p| (p.name.clone(), subst(cx, p.ty)))
        .collect();
    let ret = subst(cx, f.ret);
    let shape = if f.is_getter {
        Shape::Value(ret)
    } else {
        Shape::Method(ps, ret)
    };
    Raw {
        name,
        target: Target::Def(def),
        shape,
        own: own_generics(&f, n_owner),
        private_to: f.owner.filter(|_| f.is_private),
        optional: false,
        declared_in: f.owner,
    }
}

fn own_generics(f: &FnInfo, n_owner: usize) -> Vec<String> {
    f.generics.names.iter().skip(n_owner).cloned().collect()
}

/// Fields, then methods: own, inherited (base classes) and interface defaults.
fn adt_instance(cx: &mut Ctx, d: DefId) -> Vec<Raw> {
    let a = cx.adt(d).expect("ICE: adt");
    let n = a.generics.len();
    let fields: Vec<Raw> = a
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| Raw {
            name: f.name.clone(),
            target: Target::Field(d, i as u32),
            shape: Shape::Value(f.ty),
            own: vec![],
            private_to: f.private_to,
            optional: f.optional,
            declared_in: None,
        })
        .collect();
    let mut out = fields;
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    for name in method_names(cx, d) {
        if out.iter().any(|r| r.name == name) {
            continue;
        }
        let Some(found) = lookup_method(cx, d, &self_args, &name) else {
            continue;
        };
        if found.is_static() {
            continue;
        }
        let owner_args = found.owner_args();
        out.push(method_raw(cx, name, found.def(), &owner_args));
    }
    out
}

/// Names of the methods of `d`, its base classes and the interfaces they implement.
fn method_names(cx: &Ctx, d: DefId) -> Vec<String> {
    let mut names = vec![];
    let mut cur = Some(d);
    for _ in 0..64 {
        let Some(c) = cur else { break };
        let Some(a) = cx.adt(c) else { break };
        // Setters are reached by assignment to the property (listed through its getter).
        let mut own: Vec<&String> = a.methods.keys().filter(|n| !is_setter_key(n)).collect();
        own.sort();
        names.extend(own.into_iter().cloned());
        for b in &a.implements {
            if let Some(i) = cx.iface(b.iface) {
                names.extend(
                    i.methods
                        .iter()
                        .filter(|m| m.default.is_some() && !is_setter_key(&m.name))
                        .map(|m| m.name.clone()),
                );
            }
        }
        cur = a.base.and_then(|b| cx.class_of(b)).map(|(b, _)| b);
    }
    names
}

fn adt_statics(cx: &mut Ctx, d: DefId) -> Vec<Raw> {
    let a = cx.adt(d).expect("ICE: adt");
    let mut methods: Vec<(String, DefId)> = a
        .methods
        .iter()
        .filter(|(_, m)| m.is_static)
        .map(|(n, m)| (n.strip_prefix("static ").unwrap_or(n).to_string(), m.def))
        .collect();
    methods.sort();
    let mut statics: Vec<(String, DefId)> =
        a.statics.iter().map(|(n, g)| (n.clone(), *g)).collect();
    statics.sort();
    let mut out = vec![];
    for (name, g) in statics {
        let Some(info) = cx.global(g) else { continue };
        let private_to = info.src.owner.filter(|_| info.src.is_private);
        out.push(Raw {
            name,
            target: Target::Def(g),
            shape: Shape::Value(info.ty),
            own: vec![],
            private_to,
            optional: false,
            declared_in: None,
        });
    }
    for (name, def) in methods {
        out.push(method_raw(cx, name, def, &[]));
    }
    out
}

fn iface_members(cx: &mut Ctx, d: DefId) -> Vec<Raw> {
    let i = cx.iface(d).expect("ICE: iface");
    let mut out: Vec<Raw> = i
        .fields
        .iter()
        .enumerate()
        .map(|(k, f)| Raw {
            name: f.name.clone(),
            target: Target::Field(d, k as u32),
            shape: Shape::Value(f.ty),
            own: vec![],
            private_to: None,
            optional: f.optional,
            declared_in: None,
        })
        .collect();
    for (k, m) in i.methods.iter().enumerate() {
        if is_setter_key(&m.name) {
            continue;
        }
        let ps = m.params.iter().map(|p| (p.name.clone(), p.ty)).collect();
        let shape = if m.is_getter {
            Shape::Value(m.ret)
        } else {
            Shape::Method(ps, m.ret)
        };
        out.push(Raw {
            name: m.name.clone(),
            target: Target::IfaceMethod(d, k as u32),
            shape,
            own: vec![],
            private_to: None,
            optional: false,
            declared_in: Some(d),
        });
    }
    out
}

impl RawMembers {
    /// Phase 2: build the definitions.
    pub fn finish(self, b: &Builder) -> Members {
        let conv = |raws: Vec<Raw>| -> Vec<Member> {
            raws.into_iter()
                .filter_map(|r| {
                    Some(Member {
                        def: b.build(&r.target)?,
                        name: r.name,
                        shape: r.shape,
                        own: r.own,
                        private_to: r.private_to,
                        optional: r.optional,
                        declared_in: r.declared_in,
                    })
                })
                .collect()
        };
        let mut types = HashMap::new();
        let mut by_span = HashMap::new();
        for (d, inst, stat, kind, is_std) in self.types {
            // The first definition with a span wins: a field-only interface's struct shares
            // the interface's.
            let span = b.cx.def_spans[d.0 as usize];
            if span != Span::DUMMY {
                by_span.entry(span).or_insert(d);
            }
            types.insert(
                d,
                TypeMembers {
                    instance: conv(inst),
                    statics: conv(stat),
                    kind,
                    is_std,
                },
            );
        }
        let extensions = self
            .extensions
            .into_iter()
            .map(|(target, n, raws)| Extension {
                target,
                n,
                methods: conv(raws),
            })
            .collect();
        Members {
            types,
            by_span,
            extensions,
        }
    }
}

type Listing = Vec<(String, DefRef, String)>;

impl Members {
    /// Static members of the type definition `def`.
    pub fn statics(&self, an: &Analysis, def: &DefRef) -> Listing {
        let Some(tm) = self.by_span.get(&def.span).and_then(|d| self.types.get(d)) else {
            return vec![];
        };
        tm.statics
            .iter()
            .filter(|m| m.private_to.is_none())
            .map(|m| {
                let shown = match m.shape {
                    Shape::Value(t) if an.names.table.kind(t) == &TyKind::Error => {
                        m.def.detail.clone()
                    }
                    _ => show_shape(an, &m.shape, &|i| named_param(&m.own, i)),
                };
                (m.name.clone(), m.def.clone(), shown)
            })
            .collect()
    }

    /// Instance members of values of type `ty` (display context `ctx`).
    pub fn instance(&self, an: &Analysis, ty: TyId, ctx: u32) -> Listing {
        let table = &an.names.table;
        let ty = strip(table, ty);
        let outer = an.contexts.get(ctx as usize).cloned().unwrap_or_default();
        let mut out: Listing = vec![];
        if let TyKind::Adt(d, args) | TyKind::Dyn(d, args) = table.kind(ty) {
            if let Some(tm) = self.types.get(d) {
                let is_dyn = matches!(table.kind(ty), TyKind::Dyn(..));
                for m in &tm.instance {
                    if m.private_to.is_some() {
                        continue;
                    }
                    let param = |i: u32| -> String {
                        let i = i as usize;
                        match args.get(i) {
                            Some(a) => an.names.show_in(*a, &outer),
                            None if is_dyn && i == args.len() => an.names.show_in(ty, &outer),
                            None => named_param(&m.own, (i - args.len().min(i)) as u32),
                        }
                    };
                    push(&mut out, m, show_shape(an, &m.shape, &param));
                }
            }
        }
        for x in &self.extensions {
            let mut slots = vec![None; x.n];
            if !bind(table, x.target, ty, &mut slots) {
                continue;
            }
            for m in &x.methods {
                let param = |i: u32| -> String {
                    match slots.get(i as usize).copied().flatten() {
                        Some(t) => an.names.show_in(t, &outer),
                        // Extension methods' generics are the block's followed by their own.
                        None => named_param(&m.own, i),
                    }
                };
                push(&mut out, m, show_shape(an, &m.shape, &param));
            }
        }
        out
    }
}

fn push(out: &mut Listing, m: &Member, shown: String) {
    if !out.iter().any(|(n, _, _)| *n == m.name) {
        out.push((m.name.clone(), m.def.clone(), shown));
    }
}

fn show_shape(an: &Analysis, s: &Shape, param: &dyn Fn(u32) -> String) -> String {
    match s {
        Shape::Value(t) => an.names.show(*t, param),
        Shape::Method(ps, ret) => an.names.signature(ps, *ret, param),
    }
}

/// `T | null` and `shared<T>` offer the members of `T`.
fn strip(table: &TyTable, ty: TyId) -> TyId {
    match table.kind(ty) {
        TyKind::Option(t) | TyKind::Shared(t) => strip(table, *t),
        _ => ty,
    }
}

/// Match an `extend` target pattern (params are slots) against a concrete type.
fn bind(table: &TyTable, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) -> bool {
    let all = |ps: &[TyId], xs: &[TyId], slots: &mut [Option<TyId>]| {
        ps.len() == xs.len() && ps.iter().zip(xs).all(|(p, x)| bind(table, *p, *x, slots))
    };
    match (table.kind(pat), table.kind(actual)) {
        (TyKind::Param(i), _) => match slots.get(*i as usize) {
            Some(None) => {
                slots[*i as usize] = Some(actual);
                true
            }
            Some(Some(t)) => *t == actual,
            None => false,
        },
        (TyKind::Array(p), TyKind::Array(a))
        | (TyKind::Option(p), TyKind::Option(a))
        | (TyKind::Shared(p), TyKind::Shared(a)) => bind(table, *p, *a, slots),
        (TyKind::Map(p1, p2), TyKind::Map(a1, a2))
        | (TyKind::Promise(p1, p2), TyKind::Promise(a1, a2))
        | (TyKind::Result(p1, p2), TyKind::Result(a1, a2)) => {
            bind(table, *p1, *a1, slots) && bind(table, *p2, *a2, slots)
        }
        (TyKind::Tuple(ps), TyKind::Tuple(xs)) => all(ps, xs, slots),
        (TyKind::Adt(d, ps), TyKind::Adt(e, xs)) | (TyKind::Dyn(d, ps), TyKind::Dyn(e, xs))
            if d == e =>
        {
            all(ps, xs, slots)
        }
        _ => pat == actual,
    }
}
