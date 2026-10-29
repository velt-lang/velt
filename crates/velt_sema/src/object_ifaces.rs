//! Object types as interfaces (#1886): a value of an object type (an object literal of plain
//! values, `{ n: 1, update: () => … }`) where an interface is expected, or an `any` / `unknown`
//! holding one converted to an interface (`init as unknown as Ctx`), is the same object, as in
//! TypeScript and Node, through an implementation of the interface for the object type
//! (`Program::impls`, one per object type and interface instance):
//!
//! - an interface field reads the object's field of that name, of the same type; an optional
//!   one (`f?: T`) also a field of type `T`, or none, which reads `undefined`;
//!   Through `any`, a field of another type converts through `any` too, checked when it is
//!   read and written (`object_call_on.rs`), as Node reads whatever the object holds;
//! - a getter `get f()` and a setter `set f(v)` read and write field `f`;
//! - a method is filled by a field holding a function that takes the method's first parameters
//!   (the call passes no more, as JavaScript drops extra arguments) and returns its result, or
//!   anything for a method returning `void`; what it throws, the method throws.
//!
//! The values an `any` may hold are those the program converts to it: each object type converted
//! ([`Ctx::note_dyn_object_source`]) and each interface a dynamic value is converted to
//! ([`Ctx::note_dyn_object_target`]) are recorded while bodies are checked; once they all are,
//! [`make_dynamic_object_impls`] makes the implementation of each pair that fits, which
//! lowering's conversion finds (velt_vir `any_iface.rs`). A pair that does not fit stays a
//! `TypeError` there. An object literal whose methods use `this` (or with accessors) is a value
//! of its own interface (`body/expr/literal_iface.rs`): it converts as that interface does
//! (`crate::synth_ifaces` upcasts), so its methods get the object as `this`.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::any::Dynamic;
use crate::collect::forwarders::{synth_method, Host, Target};
use crate::collect::getters::{
    absent_getter, field_getter, field_getter_some, field_setter_taking, fixed_shape_setter,
    some_setter,
};
use crate::ctx::Ctx;
use crate::defs::{IfaceMethod, ParamSig};
use crate::hir::{Def, DefId, ExprKind as H, ImplDef, PassMode, StmtKind, TyId, TyKind};
use crate::iface_writes::SetterSrc;

/// The implementations made so far and the conversions through `any` (module docs).
#[derive(Default)]
pub(crate) struct ObjectImpls {
    /// By (object type definition, interface, interface arguments, made for a conversion
    /// through `any`); `None`: it does not fit.
    made: FxHashMap<(DefId, DefId, Vec<TyId>, bool), Option<u32>>,
    /// The object types (and literal interface values) the program converts to `any`.
    sources: Vec<TyId>,
    seen: FxHashSet<TyId>,
    /// The interfaces (with their arguments) dynamic values are converted to.
    targets: Vec<(DefId, Vec<TyId>)>,
    /// The setters of fields read and written as a plain assignment converts them
    /// (`Ctx::assign_setter`).
    pub assign_setters: FxHashSet<DefId>,
}

/// What fills one slot of the implementation.
enum Fill {
    /// The field's value (`Some`: wrapped as this optional type).
    Field(usize, TyId, Option<TyId>),
    /// Field `.0`'s value (of type `.1`), the same object as the interface field's type `.2`, of
    /// the same layout (`Upcast`).
    Layout(usize, TyId, TyId),
    /// Field `.0`'s value (of type `.1`) converted to the interface field's type `.2` as a plain
    /// assignment converts it.
    Assign(usize, TyId, TyId),
    /// Field `.0`'s value (of type `.1`) converted to the interface field's type `.2` through
    /// `any`, checked when it is read.
    Convert(usize, TyId, TyId),
    /// A field (named `.0`) the object lacks, of the optional type `.1`: `undefined`.
    Absent(String, TyId),
    /// `set f(v)` writing field `.0` (of type `.1`; `.2`: the parameter is borrowed).
    Setter(usize, TyId, bool),
    /// Method `.0` of the interface, the function in field `.1` (of type `.2`) taking `.3`
    /// arguments.
    Call(usize, usize, TyId, usize),
    /// Method `.0`, the `Function` in field `.1` (of type `.2`), called with the object as
    /// `this` (a function expression using `this`, `body/expr/this_fn_expr.rs`).
    CallOn(usize, usize, TyId),
}

/// How the implementation stores an interface field.
enum Store {
    /// In field `.0` (of type `.1`).
    Field(usize, TyId),
    /// Nowhere: the object lacks the optional field `.0` (of type `.1`).
    Absent(String, TyId),
    /// In field `.0`, of the optional field's type (`.1`) without `undefined`.
    Some(usize, TyId),
    /// In field `.0` (of type `.1`), the value (of the interface field's type `.2`) converted as
    /// a plain assignment converts it.
    Assign(usize, TyId, TyId),
    /// In field `.0` (of type `.1`), the value (of the interface field's type `.2`) converted
    /// through `any`.
    Convert(usize, TyId, TyId),
}

impl Ctx<'_> {
    /// A value of type `t` converted to `any` (module docs): the object types in it, also
    /// those of its fields, elements and options (`{ inner: { … } }`, whose `inner` an `any`
    /// may give), and a literal's own interface type.
    pub(crate) fn note_dyn_object_source(&mut self, t: TyId) {
        let mut todo = vec![t];
        while let Some(t) = todo.pop() {
            if !self.object_impls.seen.insert(t) {
                continue;
            }
            match self.ty.kind(t).clone() {
                TyKind::Adt(d, a) if a.is_empty() && self.is_object_type(t) => {
                    self.object_impls.sources.push(t);
                    let fields: Option<Vec<TyId>> =
                        self.adt(d).map(|a| a.fields.iter().map(|f| f.ty).collect());
                    todo.extend(fields.unwrap_or_default());
                }
                TyKind::Dyn(i, _) if self.literal_ifaces.contains(&i) => {
                    self.object_impls.sources.push(t);
                }
                TyKind::Array(e) | TyKind::Option(e, _) => todo.push(e),
                TyKind::Map(k, v) => todo.extend([k, v]),
                _ => {}
            }
        }
    }

    /// A dynamic value converted to `t` (module docs): recorded when it is an interface type,
    /// or an option or array of one (whose elements a view converts, `array_view_cast.rs`).
    pub(crate) fn note_dyn_object_target(&mut self, t: TyId) {
        let mut t = self.ty.opt_payload(t).unwrap_or(t);
        if let TyKind::Array(e) = *self.ty.kind(t) {
            t = self.ty.opt_payload(e).unwrap_or(e);
        }
        let TyKind::Dyn(iface, args) = self.ty.kind(t).clone() else {
            return;
        };
        if self.dynamic(t).is_some() || args.iter().any(|a| self.has_type_params(*a)) {
            return;
        }
        let known = &self.object_impls.targets;
        if !known.iter().any(|(i, a)| *i == iface && *a == args) {
            self.object_impls.targets.push((iface, args));
        }
    }

    /// The implementation of `iface<args>` for the object type `self_ty`, made on first use;
    /// `None` when its members do not give every member of the interface (module docs).
    pub(crate) fn object_iface_impl(
        &mut self,
        self_ty: TyId,
        iface: DefId,
        args: &[TyId],
    ) -> Option<u32> {
        self.object_iface_impl_of(self_ty, iface, args, false)
    }

    /// [`Self::object_iface_impl`]; `dynamic`: for a conversion through `any`, where a field of
    /// another type than the interface's converts through `any` too, checked when it is read
    /// (Node reads whatever the object holds).
    fn object_iface_impl_of(
        &mut self,
        self_ty: TyId,
        iface: DefId,
        args: &[TyId],
        dynamic: bool,
    ) -> Option<u32> {
        let TyKind::Adt(d, targs) = self.ty.kind(self_ty).clone() else {
            return None;
        };
        if !targs.is_empty() || !self.is_object_type(self_ty) {
            return None;
        }
        if args.iter().any(|a| self.has_type_params(*a)) {
            return None;
        }
        if !self.may_fill(d, iface) {
            return None;
        }
        let key = (d, iface, args.to_vec(), dynamic);
        if let Some(made) = self.object_impls.made.get(&key) {
            return *made;
        }
        let made = self
            .object_impl_plan(d, self_ty, iface, args, dynamic)
            .map(|(plan, setters)| self.build_object_impl(d, self_ty, iface, args, plan, setters));
        self.object_impls.made.insert(key, made);
        made
    }

    /// Has object type `d` a field for each member of `iface` (but optional fields)? The
    /// test by name, before the types.
    fn may_fill(&self, d: DefId, iface: DefId) -> bool {
        let (Some(i), Some(a)) = (self.iface(iface), self.adt(d)) else {
            return false;
        };
        let has = |n: &str| a.fields.iter().any(|f| f.name == n);
        (i.methods.iter()).all(|m| has(m.name.strip_prefix("set ").unwrap_or(&m.name)))
            && i.fields.iter().all(|f| f.optional || has(&f.name))
    }

    /// The slots of the implementation of `iface<args>` for object type `d`, and how it stores
    /// the interface's fields.
    fn object_impl_plan(
        &mut self,
        d: DefId,
        self_ty: TyId,
        iface: DefId,
        args: &[TyId],
        dynamic: bool,
    ) -> Option<(Vec<Fill>, Vec<Store>)> {
        let info = self.iface(iface)?;
        let (methods, ifields) = (info.methods.clone(), info.fields.clone());
        let mut full = args.to_vec();
        full.push(self_ty);
        let fields: Vec<(String, TyId)> = (self.adt(d)?.fields.iter())
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let module = self.adt(d)?.module;
        let find = |name: &str| fields.iter().position(|(n, _)| n == name);
        let mut plan = vec![];
        for (j, m) in methods.iter().enumerate() {
            if !m.generics.names.is_empty() {
                return None;
            }
            let fill = match m.name.strip_prefix("set ") {
                Some(name) => {
                    let p = m.params.first()?;
                    let want = self.subst(p.ty, &full);
                    let i = find(name).filter(|&i| self.same_ty(fields[i].1, want))?;
                    Fill::Setter(i, fields[i].1, p.mode == PassMode::Borrow)
                }
                None if m.is_getter => {
                    let want = self.subst(m.ret, &full);
                    let i = find(&m.name).filter(|&i| self.same_ty(fields[i].1, want))?;
                    Fill::Field(i, fields[i].1, None)
                }
                None => {
                    let i = find(&m.name)?;
                    match self.dynamic(fields[i].1) {
                        Some(Dynamic::Function) if self.call_on_fills(m, &full) => {
                            Fill::CallOn(j, i, fields[i].1)
                        }
                        _ => {
                            let takes = self.fn_fills_method(fields[i].1, m, &full, module)?;
                            Fill::Call(j, i, fields[i].1, takes)
                        }
                    }
                }
            };
            plan.push(fill);
        }
        let mut setters = vec![];
        for f in &ifields {
            let want = self.subst(f.ty, &full);
            let payload = self.ty.opt_payload(want);
            let fill = match find(&f.name) {
                Some(i) if self.same_ty(fields[i].1, want) => {
                    setters.push(Store::Field(i, fields[i].1));
                    Fill::Field(i, fields[i].1, None)
                }
                // An object a plain assignment keeps as the interface field's type
                // (`inner: { v: number }` for `inner: Inner`, an interface of only fields, also
                // with nested objects): the same object, so a write into a part of it reaches the
                // object, as in Node.
                Some(i) if self.assigns(fields[i].1, want, module) == Some(true) => {
                    setters.push(Store::Field(i, fields[i].1));
                    Fill::Layout(i, fields[i].1, want)
                }
                // An optional field from a field of its type without `undefined`: writing
                // `undefined` stops with a `TypeError`.
                Some(i) if f.optional && payload.is_some_and(|p| self.same_ty(fields[i].1, p)) => {
                    setters.push(Store::Some(i, want));
                    Fill::Field(i, fields[i].1, Some(want))
                }
                // Absent: a write stops with a `TypeError`, as one through `any` does.
                // (An optional `any` is `any`: absent, it holds `undefined`.)
                None if f.optional && (payload.is_some() || self.dynamic(want).is_some()) => {
                    setters.push(Store::Absent(f.name.clone(), want));
                    Fill::Absent(f.name.clone(), want)
                }
                // One a plain assignment converts both ways by making another value (an object of
                // another layout, `{ v, deep: { w } }` for `Inner` with `deep: Deep`): read and
                // written as that assignment converts it, so a part write would change a copy.
                Some(i)
                    if self.assigns(fields[i].1, want, module).is_some()
                        && self.assigns(want, fields[i].1, module).is_some() =>
                {
                    setters.push(Store::Assign(i, fields[i].1, want));
                    Fill::Assign(i, fields[i].1, want)
                }
                Some(i) if dynamic && self.converts_via_any(fields[i].1, want) => {
                    setters.push(Store::Convert(i, fields[i].1, want));
                    Fill::Convert(i, fields[i].1, want)
                }
                _ => return None,
            };
            plan.push(fill);
        }
        Some((plan, setters))
    }

    /// The number of arguments a field of type `f` takes when it fills method `m` (whose
    /// interface arguments and implementor are `full`): its parameters are the method's first
    /// ones, it has no rest parameter, throws what the method may (anything, without a
    /// `throws` clause) and returns what a plain assignment takes as the method's result (in
    /// `module`; anything for a `void` method, `Ctx::assigns`).
    fn fn_fills_method(
        &mut self,
        f: TyId,
        m: &IfaceMethod,
        full: &[TyId],
        module: usize,
    ) -> Option<usize> {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.ty.kind(f).clone()
        else {
            return None;
        };
        // What it throws is what the method throws, unless the interface declares that.
        let throws_fit = throws == self.ty.never || m.throws.is_none();
        if rest || !throws_fit || params.len() > m.params.len() {
            return None;
        }
        for (p, want) in params.iter().zip(&m.params) {
            let want = self.subst(want.ty, full);
            if !self.same_ty(*p, want) {
                return None;
            }
        }
        let want = self.subst(m.ret, full);
        let fits = want == self.ty.unit || self.assigns(ret, want, module).is_some();
        fits.then_some(params.len())
    }

    /// The implementation `plan` describes (see [`Self::object_impl_plan`]).
    fn build_object_impl(
        &mut self,
        d: DefId,
        self_ty: TyId,
        iface: DefId,
        args: &[TyId],
        plan: Vec<Fill>,
        setters: Vec<Store>,
    ) -> u32 {
        let mut full = args.to_vec();
        full.push(self_ty);
        let mut defs = vec![];
        for fill in plan {
            defs.push(match fill {
                Fill::Field(i, ty, None) => field_getter(self, d, self_ty, i, ty),
                Fill::Field(i, ty, Some(want)) => field_getter_some(self, d, self_ty, i, ty, want),
                Fill::Absent(name, want) if self.dynamic(want).is_some() => {
                    self.absent_dynamic_getter(d, self_ty, &name, want)
                }
                Fill::Absent(name, want) => absent_getter(self, d, self_ty, &name, want),
                Fill::Convert(i, ty, want) => self.convert_getter(d, self_ty, i, ty, want),
                Fill::Layout(i, ty, want) => self.layout_getter(d, self_ty, i, ty, want),
                Fill::Assign(i, ty, want) => self.assign_getter(d, self_ty, i, ty, want),
                Fill::Setter(i, ty, borrowed) => {
                    field_setter_taking(self, d, self_ty, i, ty, borrowed)
                }
                Fill::Call(j, i, fn_ty, takes) => {
                    self.fn_field_slot(d, self_ty, iface, &full, (j, i, fn_ty, takes))
                }
                Fill::CallOn(j, i, fn_ty) => {
                    let def = self.fn_field_slot(d, self_ty, iface, &full, (j, i, fn_ty, 0));
                    self.call_on_body(def, self_ty, i, fn_ty);
                    def
                }
            });
        }
        let index = self.impls.len() as u32;
        self.impls.push(ImplDef {
            ty: self_ty,
            generics: 0,
            iface,
            iface_args: args.to_vec(),
            methods: defs,
        });
        let srcs = (setters.into_iter())
            .map(|s| match s {
                Store::Field(index, ty) => SetterSrc::Field {
                    d,
                    self_ty,
                    index,
                    ty,
                },
                Store::Absent(name, ty) => SetterSrc::Method {
                    def: fixed_shape_setter(self, d, self_ty, &name, ty),
                    d,
                    name,
                },
                Store::Assign(index, ty, want) => SetterSrc::Method {
                    def: self.assign_setter(d, self_ty, index, ty, want),
                    d,
                    name: self.adt(d).expect("ICE: adt").fields[index].name.clone(),
                },
                Store::Convert(index, ty, want) => SetterSrc::Method {
                    def: self.convert_setter(d, self_ty, index, ty, want),
                    d,
                    name: self.adt(d).expect("ICE: adt").fields[index].name.clone(),
                },
                Store::Some(index, want) => SetterSrc::Method {
                    def: some_setter(self, d, self_ty, index, want),
                    d,
                    name: self.adt(d).expect("ICE: adt").fields[index].name.clone(),
                },
            })
            .collect();
        self.note_setters(index, srcs);
        // The dispatch groups gain the new methods (`throws/groups.rs`): built again on next use.
        self.groups = None;
        index
    }

    /// The method in slot `j` of `iface<full>` calling the function in field `i` (of type
    /// `fn_ty`) with the first `takes` arguments.
    fn fn_field_slot(
        &mut self,
        d: DefId,
        self_ty: TyId,
        iface: DefId,
        full: &[TyId],
        (j, i, fn_ty, takes): (usize, usize, TyId, usize),
    ) -> DefId {
        let m = self.iface(iface).expect("ICE: iface").methods[j].clone();
        let params: Vec<ParamSig> = (m.params.iter())
            .map(|p| {
                let ty = self.subst(p.ty, full);
                let mode = match self.is_copy(ty) {
                    true => PassMode::Copy,
                    false => PassMode::Borrow,
                };
                ParamSig {
                    ty,
                    mode,
                    default: None,
                    ..p.clone()
                }
            })
            .collect();
        let ret = self.subst(m.ret, full);
        let host = Host::adt(self, d);
        let name = format!(
            "{}.<fn {}>",
            self.adt(d).map_or("", |a| &a.qual_name),
            m.name
        );
        let target = Target::Field {
            index: i as u32,
            fn_ty,
            this: None,
        };
        let def = synth_method(self, &host, self_ty, name, params, ret, false, target);
        let TyKind::FnPtr { ret: gives, .. } = *self.ty.kind(fn_ty) else {
            // A `Function`: the body is `call_on_body`'s.
            return def;
        };
        // The call passes the arguments the function takes, and gives what it returns (dropped
        // for a `void` method).
        let Some(Def::Fn(f)) = &mut self.defs[def.0 as usize] else {
            panic!("ICE: a synthesized method without a body");
        };
        for s in &mut f.body.block.stmts {
            if let StmtKind::Expr(call) | StmtKind::Return(Some(call)) = &mut s.kind {
                if let H::Call { args, .. } = &mut call.kind {
                    args.truncate(takes);
                }
                call.ty = gives;
            }
        }
        // The result converted to the method's as a plain assignment converts it.
        if ret != self.ty.unit && !self.same_ty(gives, ret) {
            self.assign_result(def, d, (gives, ret), (true, "result"));
        }
        def
    }
}

/// Once every body is checked: the implementation of each interface a dynamic value is
/// converted to for each object type converted to `any` that fits (module docs).
pub(crate) fn make_dynamic_object_impls(cx: &mut Ctx) {
    let targets = std::mem::take(&mut cx.object_impls.targets);
    if targets.is_empty() {
        return;
    }
    let sources = cx.object_impls.sources.clone();
    for t in sources {
        for (iface, args) in &targets {
            match cx.ty.kind(t).clone() {
                TyKind::Dyn(have, from) => {
                    if have != *iface && cx.iface_upcast((have, &from), (*iface, args)) {
                        cx.generic_called.upcasts.insert((have, *iface));
                    }
                }
                _ => {
                    if cx.object_iface_impl(t, *iface, args).is_none() {
                        cx.object_iface_impl_of(t, *iface, args, true);
                    }
                }
            }
        }
    }
}
