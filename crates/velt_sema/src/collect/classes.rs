//! Phase 4: class hierarchy rules. `override` is required exactly when a method redefines a
//! base-class method (same signature). Vtable slots exist only for methods that some subclass
//! overrides (whole-program), laid out base slots first. Constructors are inherited when a class
//! declares none; fields without a default need a constructor that initializes them.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::forwarders::{synth_method, Host, Target};
use super::lookup::{lookup_method, Found};
use crate::ctx::Ctx;
use crate::defs::{MethodRef, RetSource};
use crate::hir::{AdtKind, DefId, TyId};

pub(super) fn check_classes(cx: &mut Ctx) {
    let adts: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.adt(*d).is_some_and(|a| a.decl.is_some()))
        .collect();
    let mut order = vec![];
    let mut seen = FxHashSet::default();
    for &d in &adts {
        base_first(cx, d, &mut seen, &mut order);
    }
    let overridden = overridden_methods(cx, &adts);
    for d in order {
        layout_vtable(cx, d, &overridden);
        inherit_ctor(cx, d);
        check_field_inits(cx, d);
    }
}

fn base_of_def(cx: &Ctx, d: DefId) -> Option<DefId> {
    let base = cx.adt(d)?.base?;
    cx.class_of(base).map(|(b, _)| b)
}

fn base_first(cx: &Ctx, d: DefId, seen: &mut FxHashSet<DefId>, order: &mut Vec<DefId>) {
    if !seen.insert(d) {
        return;
    }
    if let Some(b) = base_of_def(cx, d) {
        base_first(cx, b, seen, order);
    }
    order.push(d);
}

/// (class introducing the method, method name) for every method overridden somewhere.
fn overridden_methods(cx: &mut Ctx, adts: &[DefId]) -> Overridden {
    let mut out = BTreeSet::new();
    for &d in adts {
        let decl = cx.adt(d).and_then(|a| a.decl).expect("ICE: class decl");
        // `override #m` is reported by `own_methods`: private names are never inherited.
        for m in decl
            .methods
            .iter()
            .filter(|m| !m.decl.sig.name.is_private_name())
        {
            let name = &m.decl.sig.name;
            let module = cx.adt(d).expect("ICE: adt").module;
            let key = cx.method_key(module, name, m.is_setter);
            if !m.is_override && (m.is_static || !std_override(cx, d, &key)) {
                continue;
            }
            match introducer(cx, d, &key) {
                // Generic methods get no vtable slot: calls through a base class are rejected
                // (`body::expr::method_call`), calls on the concrete class are direct.
                Some(_) if !m.decl.sig.generics.is_empty() => {
                    cx.generic_overrides.push((d, key));
                }
                Some(origin) => {
                    // A getter-only override shadows the base's setter too (JS replaces the
                    // whole property), so that setter becomes virtual.
                    if let Some(set_key) = shadowed_setter(cx, d, m) {
                        if let Some(o) = introducer(cx, d, &set_key) {
                            out.insert((o, set_key));
                        }
                    }
                    out.insert((origin, key));
                }
                None => cx.err(
                    format!(
                        "method `{}` is marked `override` but no base class has a method `{}`",
                        name.name, name.name
                    ),
                    name.span,
                ),
            }
        }
    }
    out
}

/// Does `d`'s method `key` redefine one a std class declares (`Error.toString`)? TypeScript
/// code redefines its lib's methods without `override`, so such a redefinition overrides
/// without it.
fn std_override(cx: &Ctx, d: DefId, key: &str) -> bool {
    introducer(cx, d, key).is_some_and(|o| cx.declared_in_std(o))
}

/// The ancestor of `d` (excluding `d`) that first declares the instance method `name` without
/// `override`, or that gets it as the default method of an interface it implements (static
/// methods of the name are skipped).
fn introducer(cx: &Ctx, d: DefId, name: &str) -> Option<DefId> {
    let mut cur = base_of_def(cx, d);
    let mut found = None;
    let mut guard = 0;
    while let Some(c) = cur {
        guard += 1;
        if guard > 64 {
            break;
        }
        let a = cx.adt(c)?;
        // A static of the name is another namespace: look past it, as method lookup does.
        let instance = a.methods.get(name).filter(|m| !m.is_static);
        if instance.is_some() {
            let is_override = a.decl.is_some_and(|t| {
                t.methods.iter().any(|x| {
                    x.is_override && cx.method_key(a.module, &x.decl.sig.name, x.is_setter) == name
                })
            });
            found = Some(c);
            if !is_override {
                return found;
            }
        } else if has_default(cx, c, name) {
            return Some(c);
        }
        cur = base_of_def(cx, c);
    }
    found
}

/// Does class `c` get `name` as a default method of an interface it implements?
fn has_default(cx: &Ctx, c: DefId, name: &str) -> bool {
    cx.adt(c).is_some_and(|a| {
        a.implements.iter().any(|b| {
            cx.iface(b.iface).is_some_and(|i| {
                i.methods
                    .iter()
                    .any(|m| m.name == name && m.default.is_some())
            })
        })
    })
}

/// A class's vtable while it is being laid out.
struct Vtable {
    entries: Vec<DefId>,
    slots: FxHashMap<String, u32>,
}

impl Vtable {
    fn push(&mut self, name: String, def: DefId) {
        self.slots.insert(name, self.entries.len() as u32);
        self.entries.push(def);
    }
}

type Overridden = BTreeSet<(DefId, String)>;

fn layout_vtable(cx: &mut Ctx, d: DefId, overridden: &Overridden) {
    let a = cx.adt(d).expect("ICE: adt");
    let n = a.generics.len();
    let mut vt = match a.base.and_then(|b| cx.class_of(b)) {
        Some((b, _)) => {
            let ba = cx.adt(b).expect("ICE: base");
            Vtable {
                entries: ba.vtable.clone(),
                slots: ba.vslots.clone(),
            }
        }
        None => Vtable {
            entries: vec![],
            slots: FxHashMap::default(),
        },
    };
    own_methods(cx, d, overridden, &mut vt);
    getter_only_overrides(cx, d, &mut vt);
    // Interface default methods this class introduces and a subclass overrides.
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    // A range, not a scan: every class would otherwise walk every overridden method.
    let introduced: Vec<String> = overridden
        .range((d, String::new())..)
        .take_while(|(c, _)| *c == d)
        .filter(|(_, name)| !vt.slots.contains_key(name))
        .map(|(_, name)| name.clone())
        .collect();
    for name in introduced {
        if let Some(fwd) = default_forwarder(cx, d, &self_args, &name) {
            vt.push(name, fwd);
        }
    }
    mark_dispatched(cx, &vt.entries);
    let a = cx.adt_mut(d);
    a.vtable = vt.entries;
    a.vslots = vt.slots;
}

/// `override` rules for `d`'s own methods; overrides replace slots, overridden ones get one.
fn own_methods(cx: &mut Ctx, d: DefId, overridden: &Overridden, vt: &mut Vtable) {
    let a = cx.adt(d).expect("ICE: adt");
    let (decl, base_ty, module) = (a.decl.expect("ICE: adt decl"), a.base, a.module);
    for m in decl.methods.iter().filter(|m| !m.is_static) {
        let name = &m.decl.sig.name;
        let key = cx.method_key(module, name, m.is_setter);
        let Some(mref) = cx.adt(d).and_then(|a| a.methods.get(&key)).copied() else {
            continue;
        };
        if name.name.starts_with(ast::PRIVATE_NAME_PREFIX) {
            // `#m` is never inherited, so it overrides nothing and has no vtable slot.
            if m.is_override {
                cx.error(
                    Diagnostic::error(
                        format!(
                            "`override` on `{}`: private names are never inherited",
                            name.name
                        ),
                        name.span,
                    )
                    .with_note("remove `override`"),
                );
            }
            continue;
        }
        let inherited = base_ty
            .and_then(|b| cx.class_of(b))
            .and_then(|(b, bargs)| lookup_method(cx, b, &bargs, &key))
            .filter(|f| !f.is_static());
        if m.is_override || (inherited.is_some() && std_override(cx, d, &key)) {
            if let Some(f) = inherited {
                check_override_sig(cx, mref, &f, name);
            }
            if let Some(&slot) = vt.slots.get(&key) {
                vt.entries[slot as usize] = mref.def;
            }
        } else if inherited.is_some() {
            cx.error(
                Diagnostic::error(
                    format!("method `{}` redefines a base class method", name.name),
                    name.span,
                )
                .with_note(format!("mark it `override {}(...)`", name.name)),
            );
        } else if overridden.contains(&(d, key.clone())) {
            vt.push(key, mref.def);
        }
    }
}

/// The setter key `set:name` when method `m` of class `d` is a getter and `d` declares no
/// setter of that name: overriding the getter then replaces the base's setter as well.
fn shadowed_setter(cx: &Ctx, d: DefId, m: &ast::Method) -> Option<String> {
    let a = cx.adt(d)?;
    let decl = a.decl?;
    let name = &m.decl.sig.name;
    let own_setter = decl
        .methods
        .iter()
        .any(|s| s.is_setter && !s.is_static && s.decl.sig.name.name == name.name);
    (m.is_getter && !m.is_static && !own_setter).then(|| cx.method_key(a.module, name, true))
}

/// For every getter `d` overrides without declaring the setter its base has, a setter of `d`
/// that throws Node's `TypeError` (in JS the getter-only accessor replaces the whole
/// property), registered as `d`'s own method and in the base setter's vtable slot.
fn getter_only_overrides(cx: &mut Ctx, d: DefId, vt: &mut Vtable) {
    let a = cx.adt(d).expect("ICE: adt");
    let (decl, base_ty, n) = (a.decl.expect("ICE: adt decl"), a.base, a.generics.len());
    let Some((b, bargs)) = base_ty.and_then(|b| cx.class_of(b)) else {
        return;
    };
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let self_ty = cx.ty.intern(crate::hir::TyKind::Adt(d, self_args));
    for m in decl.methods.iter().filter(|m| m.is_override) {
        hidden_getter(cx, d, (b, &bargs), m);
        let Some(key) = shadowed_setter(cx, d, m) else {
            continue;
        };
        let Some(f) = lookup_method(cx, b, &bargs, &key).filter(|f| !f.is_static()) else {
            continue;
        };
        let Some(pty) = cx.fn_info(f.def()).params.first().map(|p| p.ty) else {
            continue;
        };
        let ty = cx.subst(pty, &f.owner_args());
        let name = &m.decl.sig.name.name;
        let def = super::getters::getter_only_setter(cx, d, self_ty, name, ty);
        cx.getter_only_setters.insert(def);
        let r = MethodRef {
            def,
            is_static: false,
        };
        cx.adt_mut(d).methods.insert(key.clone(), r);
        if let Some(&slot) = vt.slots.get(&key) {
            vt.entries[slot as usize] = def;
        }
    }
}

/// An error when method `m` of class `d` overrides only the setter of a `get`/`set` pair of
/// its base `b`: in JavaScript the setter-only accessor replaces the whole property, so reads
/// give `undefined`, which the getter's type can't hold.
fn hidden_getter(cx: &mut Ctx, d: DefId, (b, bargs): (DefId, &[TyId]), m: &ast::Method) {
    let Some(a) = cx.adt(d) else { return };
    let (Some(decl), module) = (a.decl, a.module) else {
        return;
    };
    let name = &m.decl.sig.name;
    let own_getter = decl
        .methods
        .iter()
        .any(|g| g.is_getter && !g.is_static && g.decl.sig.name.name == name.name);
    if !m.is_setter || m.is_static || own_getter {
        return;
    }
    let key = cx.method_key(module, name, false);
    let inherited = lookup_method(cx, b, bargs, &key)
        .filter(|f| matches!(f, Found::Class { .. }) && !f.is_static())
        .is_some_and(|f| cx.fn_info(f.def()).is_getter);
    if !inherited {
        return;
    }
    let n = &name.name;
    cx.error(
        Diagnostic::error(
            format!("`set {n}` overrides only the setter of the base class's `get {n}`/`set {n}` pair"),
            name.span,
        )
        .with_note("in JavaScript a setter-only accessor replaces the whole property, so reading it would give `undefined`")
        .with_note(format!("override the getter too (`override get {n}()`)")),
    );
}

/// Vtable methods have a fixed ABI (no ownership inference).
fn mark_dispatched(cx: &mut Ctx, entries: &[DefId]) {
    for &def in entries {
        cx.fn_info_mut(def).fixed_modes = true;
    }
}

/// A class method of `d` forwarding to the interface default `name` it inherits (so the class
/// vtable holds a class method), registered as `d`'s own method.
fn default_forwarder(cx: &mut Ctx, d: DefId, self_args: &[TyId], name: &str) -> Option<DefId> {
    let Some(Found::Default {
        def,
        iface,
        iface_args,
        implementor,
    }) = lookup_method(cx, d, self_args, name)
    else {
        return None;
    };
    let m = cx
        .iface(iface)?
        .methods
        .iter()
        .find(|m| m.name == name)?
        .clone();
    let params = m
        .params
        .iter()
        .map(|p| {
            let mut p = p.clone();
            p.ty = cx.subst(p.ty, &iface_args);
            p
        })
        .collect();
    let ret = cx.subst(m.ret, &iface_args);
    let mut targs = iface_args;
    targs.push(implementor);
    let qual = cx.adt(d)?.qual_name.clone();
    let host = Host::adt(cx, d);
    let mut_this = m.mut_this;
    let fwd = synth_method(
        cx,
        &host,
        implementor,
        format!("{qual}.{name}"),
        params,
        ret,
        mut_this,
        Target::Default(def, targs),
    );
    let r = MethodRef {
        def: fwd,
        is_static: false,
    };
    cx.adt_mut(d).methods.insert(name.to_string(), r);
    Some(fwd)
}

/// May an override returning `this` (the class `own`) give the base method's result `base`:
/// is `base` a class that `own` extends?
fn this_result_ok(cx: &mut Ctx, own: TyId, base: TyId) -> bool {
    let mut cur = own;
    while let Some(next) = cx.base_of(cur) {
        if next == base {
            return true;
        }
        cur = next;
    }
    false
}

fn check_override_sig(cx: &mut Ctx, m: MethodRef, base: &Found, name: &velt_syntax::ast::Ident) {
    let (mp, mr, mg) = {
        let f = cx.fn_info(m.def);
        let ps = f.params.iter().map(|p| p.ty).collect::<Vec<_>>();
        (ps, f.ret, f.is_getter)
    };
    let (bp, br, bg) = {
        let f = cx.fn_info(base.def());
        let ps = f.params.iter().map(|p| p.ty).collect::<Vec<_>>();
        (ps, f.ret, f.is_getter)
    };
    let mut args = base.owner_args();
    // Own type params of generic methods: the base's `Param(nb + k)` is the override's
    // `Param(nd + k)`.
    let (own_m, own_b) = (own_generics(cx, m.def), own_generics(cx, base.def()));
    if own_m != own_b {
        cx.err(
            format!(
                "method `{}` must have {own_b} type parameter(s) like the base class method it overrides",
                name.name
            ),
            name.span,
        );
        return;
    }
    let nd = cx.fn_info(m.def).generics.len() - own_m;
    args.extend((0..own_m as u32).map(|k| cx.ty.param(nd as u32 + k)));
    let bp: Vec<TyId> = bp.into_iter().map(|t| cx.subst(t, &args)).collect();
    let br = cx.subst(br, &args);
    let message = format!(
        "method `{}` does not have the same signature as the base class method it overrides",
        name.name
    );
    // A call through the base's type converts the result of a `this`-returning method to the
    // receiver's class (`body::expr::this_type`), so its overrides must return `this` too.
    if cx.fn_info(base.def()).returns_this() && !cx.fn_info(m.def).returns_this() {
        if cx.fn_info(m.def).ret_source == RetSource::Body {
            cx.fn_info_mut(m.def).inherits_this = true;
        } else {
            cx.error(
                Diagnostic::error(message, name.span).with_note(
                    "TypeScript's TS2416: the base class method returns `this`, the class of the object it is called on; declare the override's result as `this` and return `this`",
                ),
            );
            return;
        }
    }
    let at = (name.span, message.clone());
    // `override m(): this`: the override returns its receiver, typed as the base method's
    // result, which a call converts to the receiver's class (`body::expr::this_type`).
    let mr = match cx.fn_info(m.def).returns_this() && mr != br && this_result_ok(cx, mr, br) {
        true => {
            cx.fn_info_mut(m.def).ret = br;
            br
        }
        false => mr,
    };
    let ret_ok = super::ret_infer::override_ret(cx, (m.def, base.def()), &args, (mr, br), at);
    if !cx.same_tys(&mp, &bp) || !ret_ok || mg != bg {
        cx.err(message, name.span);
    }
}

/// Number of a method's own type params (after its owner's).
fn own_generics(cx: &Ctx, def: DefId) -> usize {
    let f = cx.fn_info(def);
    let owner = f
        .owner
        .and_then(|o| cx.adt(o))
        .map_or(0, |a| a.generics.len());
    f.generics.len().saturating_sub(owner)
}

fn inherit_ctor(cx: &mut Ctx, d: DefId) {
    let a = cx.adt(d).expect("ICE: adt");
    let own = a.own_ctor;
    if a.kind != AdtKind::Class {
        if let Some(c) = own {
            let span = cx.fn_info(c).name_span;
            cx.err(
                "structs cannot have constructors; use a struct literal",
                span,
            );
        }
        return;
    }
    let inherited = base_of_def(cx, d).and_then(|b| cx.adt(b).and_then(|a| a.ctor));
    cx.adt_mut(d).ctor = own.or(inherited);
}

/// Fields without a default need a constructor of this class to initialize them.
fn check_field_inits(cx: &mut Ctx, d: DefId) {
    let a = cx.adt(d).expect("ICE: adt");
    if a.kind != AdtKind::Class || a.own_ctor.is_some() {
        return;
    }
    let missing: Vec<(String, velt_common::Span)> = a.fields[a.own_fields_start..]
        .iter()
        .filter(|f| !f.has_default)
        .map(|f| (f.name.clone(), f.span))
        .collect();
    let class = a.name.clone();
    for (name, span) in missing {
        cx.error(
            Diagnostic::error(
                format!("field `{name}` of class `{class}` has no default value"),
                span,
            )
            .with_note("give it a default (`= value`) or initialize it in a `constructor`"),
        );
    }
}
