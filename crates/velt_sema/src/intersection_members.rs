//! Members of intersections that a type parameter keeps from being reduced to one object type
//! (#350, #1304), as TypeScript has them:
//!
//! - a field that is a type parameter in one part (`{ a: D } & { a: X }`) is `D` when `D`'s
//!   constraint already is an `X`, else the deferred `D & X` ([`Ctx::meet_param`]);
//! - two function types are overloads; when one takes every argument the other does, a call
//!   through either is a call of that one ([`Ctx::takes_all_of`]);
//! - in a generic body, a member of a part that is an object type (`s.$set(v)` for
//!   `s: T & { $set: (v: T) => void }`) is read, written and called through the getter and
//!   setter slots of an interface made of that part ([`Ctx::part_field`]), as `t.x` is for a
//!   `T extends I`. Each object type such an intersection reduces to implements it
//!   ([`Ctx::link_part_ifaces`]), so a read shares the field's value and a write changes the
//!   object itself.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FieldInfo, Generics, IfaceInfo};
use crate::hir::{DefId, ImplDef, TyId, TyKind, TyOp};
use crate::iface_writes::SetterSrc;

/// The interfaces made of intersections' object parts (`Ctx::part_field`).
#[derive(Default)]
pub(crate) struct PartIfaces {
    /// Per part, as written over its own type parameters: the interface and those parameters
    /// (the interface's arguments, in its parameter order).
    by_part: FxHashMap<TyId, (DefId, Vec<u32>)>,
    /// The interfaces, in creation order: (interface, part over the interface's parameters).
    list: Vec<(DefId, TyId)>,
    /// (interface, object type def) pairs implemented.
    linked: FxHashSet<(DefId, DefId)>,
}

/// A member of an intersection's object part, read through its interface's slots.
pub(crate) struct PartField {
    pub iface: DefId,
    pub args: Vec<TyId>,
    pub index: usize,
    pub ty: TyId,
    pub readonly: bool,
}

impl Ctx<'_> {
    /// A field that is a type parameter in one part (`{ a: D } & { a: X }`), as TypeScript
    /// intersects it: `D` itself when `D`'s constraint already is an `X` (`D extends string`
    /// with `X = string`), else, when `X` is an object type or a type parameter, the deferred
    /// `D & X`, reduced once `D` is known.
    pub(crate) fn meet_param(&mut self, a: TyId, b: TyId, module: usize) -> Option<TyId> {
        let param = |cx: &Self, t: TyId| match cx.ty.kind(t) {
            TyKind::Param(n) => Some(*n),
            _ => None,
        };
        let (n, other) = match (param(self, a), param(self, b)) {
            (Some(n), _) => (n, b),
            (None, Some(n)) => (n, a),
            (None, None) => return None,
        };
        let p = self.ty.param(n);
        if let Some(c) = self.constraint_of(n) {
            let members = self.union_members(c).unwrap_or_else(|| vec![c]);
            let within = |cx: &mut Self, m: TyId| {
                m == other || cx.lit_value(m).is_some_and(|v| cx.lit_base(&v) == other)
            };
            if members.iter().all(|&m| within(self, m)) {
                return Some(p);
            }
        }
        if !(self.is_object_type(other) || self.deferred_part(other)) {
            return None;
        }
        let op = self.ty.intern(TyKind::Op(TyOp::Intersect, vec![a, b]));
        self.type_level.op_modules.entry(op).or_insert(module);
        Some(op)
    }

    /// Are `wide` and `narrow` function types with the same result, where each of `wide`'s
    /// parameters is `narrow`'s or an object type with a subset of its fields?
    pub(crate) fn takes_all_of(&mut self, wide: TyId, narrow: TyId) -> bool {
        let (
            TyKind::FnPtr {
                params: pw,
                ret: rw,
                throws: tw,
                rest: xw,
            },
            TyKind::FnPtr {
                params: pn,
                ret: rn,
                throws: tn,
                rest: xn,
            },
        ) = (self.ty.kind(wide).clone(), self.ty.kind(narrow).clone())
        else {
            return false;
        };
        // A written signature's error set may still be a variable (`throws::vars`): the same
        // but for it is the same.
        if pw.len() != pn.len() || rw != rn || xw != xn || !self.same_ty(tw, tn) {
            return false;
        }
        pw.iter().zip(&pn).all(|(&w, &n)| {
            // A parameter of type `any` takes whatever the other takes.
            if w == n || self.dynamic(w) == Some(crate::any::Dynamic::Any) {
                return true;
            }
            if !(self.is_object_type(w) && self.is_object_type(n)) {
                return false;
            }
            let have = self.public_fields(n);
            self.public_fields(w).iter().all(|f| {
                have.iter()
                    .any(|g| g.name == f.name && g.ty == f.ty && (f.optional || !g.optional))
            })
        })
    }

    /// Is `b`, a type parameter's bound, a function type the parameter is taken to be exactly
    /// (`F extends (x: number) => number`; issue #1212): no type parameters, no rest parameter
    /// and nothing dynamic, unlike the "any function" bound `(...args: any[]) => any`, under
    /// which a parameter keeps the argument's own function type.
    pub(crate) fn exact_fn_bound(&self, b: TyId) -> bool {
        let TyKind::FnPtr {
            params, ret, rest, ..
        } = self.ty.kind(b)
        else {
            return false;
        };
        !*rest
            && !self.has_type_params(b)
            && params
                .iter()
                .chain(std::iter::once(ret))
                .all(|&t| self.dynamic(t).is_none())
    }

    /// The constraint (`T extends string`) of type parameter `n` of the function being checked.
    pub(crate) fn constraint_of(&self, n: u32) -> Option<TyId> {
        let d = *self.checking.last()?;
        let f = self.try_fn(d)?;
        f.generics.constraints.get(n as usize).copied().flatten()
    }

    /// Is `t` a deferred intersection with a type parameter part whose other parts hold no
    /// deferred type (`T & { $set: (v: T) => void }`): an argument fixes `T` through it
    /// (`Ctx::match_ty`).
    pub(crate) fn intersection_pattern(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Op(TyOp::Intersect, parts) => {
                parts
                    .iter()
                    .any(|&p| matches!(self.ty.kind(p), TyKind::Param(_)))
                    && parts.iter().all(|&p| {
                        matches!(self.ty.kind(p), TyKind::Param(_)) || !self.mentions_op(p)
                    })
            }
            _ => false,
        }
    }

    /// Member `name` of `t`, an intersection with a part that is a type parameter, found in one
    /// of its object parts: the interface made of that part (its arguments are the type
    /// parameters the part mentions) and the field's index there.
    pub(crate) fn part_field(&mut self, t: TyId, name: &str) -> Option<PartField> {
        let TyKind::Op(TyOp::Intersect, parts) = self.ty.kind(t).clone() else {
            return None;
        };
        if !parts.iter().any(|&p| self.deferred_part(p)) {
            return None;
        }
        for p in parts {
            if !self.is_object_type(p) {
                continue;
            }
            let fields = self.public_fields(p);
            let Some(index) = fields.iter().position(|f| f.name == name) else {
                continue;
            };
            let (iface, params) = self.part_iface(p);
            let f = &fields[index];
            return Some(PartField {
                iface,
                args: params.iter().map(|&n| self.ty.param(n)).collect(),
                index,
                ty: f.ty,
                readonly: f.readonly,
            });
        }
        None
    }

    /// The interface with the fields of object type `part` (over the type parameters `part`
    /// mentions, which are its arguments), made once per part.
    fn part_iface(&mut self, part: TyId) -> (DefId, Vec<u32>) {
        if let Some(found) = self.part_ifaces.by_part.get(&part) {
            return found.clone();
        }
        let mut params: Vec<u32> = vec![];
        crate::types::collect_params(&self.ty, part, &mut params);
        let own: Vec<TyId> = (0..params.len() as u32).map(|i| self.ty.param(i)).collect();
        let renumber = |cx: &mut Self, t: TyId| {
            cx.ty.map(t, &mut |k| match k {
                TyKind::Param(i) => params.iter().position(|p| p == i).map(|j| own[j]),
                _ => None,
            })
        };
        let shown = self.display(part);
        let mut generics = Generics::default();
        for i in 0..params.len() {
            generics.push(&format!("T{i}"));
        }
        let fields: Vec<FieldInfo> = self
            .public_fields(part)
            .into_iter()
            .map(|f| {
                let ty = renumber(self, f.ty);
                FieldInfo {
                    name: f.name,
                    ty,
                    declared: ty,
                    span: Span::DUMMY,
                    readonly: f.readonly,
                    optional: f.optional,
                    has_default: false,
                    default: None,
                    default_throws: vec![],
                    private_to: None,
                }
            })
            .collect();
        let module = match self.ty.kind(part) {
            TyKind::Adt(d, _) => self.adt(*d).map_or(0, |a| a.module),
            _ => 0,
        };
        let iface = self.alloc_def(
            Span::DUMMY,
            DefInfo::Iface(Box::new(IfaceInfo {
                name: shown.clone(),
                qual_name: shown,
                module,
                span: Span::DUMMY,
                generics,
                fields,
                methods: vec![],
                parents: vec![],
                decl: None,
            })),
        );
        let generic_part = renumber(self, part);
        self.part_ifaces
            .by_part
            .insert(part, (iface, params.clone()));
        self.part_ifaces.list.push((iface, generic_part));
        // The object types reduced before this interface existed implement it too.
        let done: Vec<TyId> = self.type_level.reduced.values().copied().collect();
        for r in done {
            self.link_part_iface(iface, generic_part, r);
        }
        (iface, params)
    }

    /// `r`, an object type an intersection reduced to, implements every part interface whose
    /// fields it has.
    pub(crate) fn link_part_ifaces(&mut self, r: TyId) {
        for (iface, part) in self.part_ifaces.list.clone() {
            self.link_part_iface(iface, part, r);
        }
    }

    /// The impl of part interface `iface` (made of `part`, over the interface's parameters)
    /// for object type `r`'s definition, over that definition's own parameters, when `r`
    /// has the part's fields: one getter per field and the setters of the writable ones.
    fn link_part_iface(&mut self, iface: DefId, part: TyId, r: TyId) {
        let TyKind::Adt(d, _) = self.ty.kind(r).clone() else {
            return;
        };
        let Some(a) = self.adt(d) else {
            return;
        };
        let m = a.generics.len() as u32;
        if !self.is_object_type(r) || self.part_ifaces.linked.contains(&(iface, d)) {
            return;
        }
        let own: Vec<TyId> = (0..m).map(|i| self.ty.param(i)).collect();
        let self_ty = self.ty.intern(TyKind::Adt(d, own));
        let have = self.public_fields(self_ty);
        let want = self.public_fields(part);
        let n = self.iface(iface).map_or(0, |i| i.generics.len());
        let mut slots: Vec<Option<TyId>> = vec![None; n];
        let mut found = vec![];
        for f in &want {
            let Some(k) = have.iter().position(|g| g.name == f.name) else {
                return;
            };
            if !self.match_ty(f.ty, have[k].ty, &mut slots) {
                return;
            }
            found.push((k, have[k].ty));
        }
        let Some(args) = slots.into_iter().collect::<Option<Vec<TyId>>>() else {
            return;
        };
        for (f, &(_, ty)) in want.iter().zip(&found) {
            if self.subst(f.ty, &args) != ty {
                return;
            }
        }
        self.part_ifaces.linked.insert((iface, d));
        let methods = found
            .iter()
            .map(|&(k, ty)| crate::collect::getters::field_getter(self, d, self_ty, k, ty))
            .collect();
        let setters = self
            .setter_fields(iface)
            .iter()
            .map(|&j| SetterSrc::Field {
                d,
                self_ty,
                index: found[j].0,
                ty: found[j].1,
            })
            .collect();
        self.impls.push(ImplDef {
            ty: self_ty,
            generics: m,
            iface,
            iface_args: args,
            methods,
        });
        let index = (self.impls.len() - 1) as u32;
        self.note_setters(index, setters);
    }
}
