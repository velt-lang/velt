//! Phase 4: class hierarchy rules. `override` is required exactly when a method redefines a
//! base-class method (same signature). Vtable slots exist only for methods that some subclass
//! overrides (whole-program), laid out base slots first. Constructors are inherited when a class
//! declares none; fields without a default need a constructor that initializes them.

use std::collections::{BTreeSet, HashMap, HashSet};

use velt_common::Diagnostic;

use super::forwarders::{synth_method, Host, Target};
use super::lookup::{lookup_method, Found};
use crate::ctx::Ctx;
use crate::defs::{member_key, MethodRef};
use crate::hir::{AdtKind, DefId, TyId};

pub(super) fn check_classes(cx: &mut Ctx) {
    let adts: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.adt(*d).is_some_and(|a| a.decl.is_some()))
        .collect();
    let mut order = vec![];
    let mut seen = HashSet::new();
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

fn base_first(cx: &Ctx, d: DefId, seen: &mut HashSet<DefId>, order: &mut Vec<DefId>) {
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
        for m in decl.methods.iter().filter(|m| m.is_override) {
            let name = &m.decl.sig.name;
            let key = member_key(&name.name, m.is_setter);
            match introducer(cx, d, &key) {
                // Generic methods get no vtable slot: calls through a base class are rejected
                // (`body::expr::method_call`), calls on the concrete class are direct.
                Some(_) if !m.decl.sig.generics.is_empty() => {
                    cx.generic_overrides.push((d, key));
                }
                Some(origin) => {
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

/// The ancestor of `d` (excluding `d`) that first declares `name` without `override`, or
/// that gets it as the default method of an interface it implements.
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
        if let Some(m) = a.methods.get(name) {
            if m.is_static {
                return None;
            }
            let is_override = a.decl.is_some_and(|t| {
                t.methods.iter().any(|x| {
                    x.is_override && member_key(&x.decl.sig.name.name, x.is_setter) == name
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
    slots: HashMap<String, u32>,
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
            slots: HashMap::new(),
        },
    };
    own_methods(cx, d, overridden, &mut vt);
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
    let (decl, base_ty) = (a.decl.expect("ICE: adt decl"), a.base);
    for m in decl.methods.iter().filter(|m| !m.is_static) {
        let name = &m.decl.sig.name;
        let key = member_key(&name.name, m.is_setter);
        let Some(mref) = cx.adt(d).and_then(|a| a.methods.get(&key)).copied() else {
            continue;
        };
        let inherited = base_ty
            .and_then(|b| cx.class_of(b))
            .and_then(|(b, bargs)| lookup_method(cx, b, &bargs, &key))
            .filter(|f| !f.is_static());
        if m.is_override {
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
            p.ty = cx.ty.subst(p.ty, &iface_args);
            p
        })
        .collect();
    let ret = cx.ty.subst(m.ret, &iface_args);
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
    let bp: Vec<TyId> = bp.into_iter().map(|t| cx.ty.subst(t, &args)).collect();
    let br = cx.ty.subst(br, &args);
    if mp != bp || mr != br || mg != bg {
        cx.err(
            format!(
                "method `{}` does not have the same signature as the base class method it overrides",
                name.name
            ),
            name.span,
        );
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
