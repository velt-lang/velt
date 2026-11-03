//! The methods a call through an interface, a virtual method or a type parameter's method may
//! run, for [`super::changes`] (#854): the class's own method and its subclasses' overrides, or
//! the implementations of that interface method. An object type implements an interface with
//! the functions its literals store in the field of the method's name (`{ render(c) { … } }`,
//! sema's `Plugin literal` types, `object_ifaces.rs`): those are followed when every literal of the type stores a closure or a
//! named function there. When the set of methods is not known completely here (a literal
//! storing another value, a spread making the object, an implementation of another kind, an
//! `any` converted to the interface), there
//! are no targets and the caller stays conservative.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyKind};
use crate::ownership::local_closures::walk::{self, Visit};

/// A method a call may run, and how many of the call's arguments it does not take first (1
/// for a literal's function, which takes no `this`).
pub(super) type Target = (DefId, usize);

/// The functions the literals of each object type store in each field: `None` when one stores
/// a value that is not a closure or a named function, or a spread makes such an object.
#[derive(Default)]
pub(super) struct Literals {
    fields: FxHashMap<(DefId, u32), Option<Vec<DefId>>>,
    /// Object types a spread makes: their fields hold what was copied.
    spread: Vec<DefId>,
    /// Interfaces the program converts an `any` to: what it holds may implement them in ways
    /// not listed here.
    from_any: Vec<DefId>,
}

impl Literals {
    /// Field `k` of object type `d` gets `v`: a closure or a named function is followed, any
    /// other value makes the field's functions unknown.
    fn store(&mut self, d: DefId, k: u32, v: &Expr) {
        let f = match v.kind {
            E::Closure(c) => Some(c),
            E::FnRef(g, _) => Some(g),
            _ => None,
        };
        let entry = self.fields.entry((d, k)).or_insert_with(|| Some(vec![]));
        match (entry.as_mut(), f) {
            (Some(list), Some(g)) => list.push(g),
            _ => *entry = None,
        }
    }

    /// Scan the program's bodies (before any is taken out of `cx.defs`).
    pub(super) fn scan(cx: &Ctx) -> Literals {
        let mut out = Literals::default();
        for (i, d) in cx.defs.iter().enumerate() {
            match d {
                Some(Def::Fn(f)) => walk::block(&f.body.block, &mut Scan(cx, &mut out)),
                Some(Def::Global(g)) => walk::expr(&g.init, &mut Scan(cx, &mut out)),
                // A module constant's initializer is kept with its info until the end.
                _ => {
                    if let Some(init) = cx.global(DefId(i as u32)).and_then(|g| g.init.as_ref()) {
                        walk::expr(init, &mut Scan(cx, &mut out));
                    }
                }
            }
        }
        out
    }
}

struct Scan<'a>(&'a Ctx<'a>, &'a mut Literals);

impl Visit for Scan<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::AdtLit { def, fields, .. } => {
                if !self.0.adt(*def).is_some_and(object_type) {
                    return;
                }
                for (k, v) in fields.iter().enumerate() {
                    self.1.store(*def, k as u32, v);
                }
            }
            // A field of an object type assigned later (`obj.render = (c) => …`).
            E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                let E::Field { base, index, .. } = &place.kind else {
                    return;
                };
                let t = match self.0.ty.kind(base.ty) {
                    TyKind::Option(inner, _) => *inner,
                    _ => base.ty,
                };
                if let TyKind::Adt(d, _) = self.0.ty.kind(t) {
                    if self.0.adt(*d).is_some_and(object_type) {
                        self.1.store(*d, *index, value);
                    }
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::AnyTo),
                ..
            } => {
                let t = match self.0.ty.kind(e.ty) {
                    TyKind::Option(inner, _) => *inner,
                    _ => e.ty,
                };
                if let TyKind::Dyn(iface, _) = self.0.ty.kind(t) {
                    self.1.from_any.push(*iface);
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::SpreadObject),
                ..
            } => {
                if let TyKind::Adt(d, _) = self.0.ty.kind(e.ty) {
                    self.1.spread.push(*d);
                }
            }
            _ => {}
        }
    }
}

/// The methods the call of `callee` with `args` (the receiver first) may run; `None` when they
/// are not known completely here.
pub(super) fn method_targets(
    cx: &Ctx,
    lits: &Literals,
    callee: &Callee,
    args: &[Expr],
) -> Option<Vec<Target>> {
    let receiver = || args.first().map(|a| cx.ty.kind(a.ty).clone());
    let out = match callee {
        Callee::ParamMethod { iface, slot, .. } => implementations(cx, lits, *iface, *slot)?,
        Callee::Dyn { slot, .. } => match receiver() {
            Some(TyKind::Dyn(iface, _)) => implementations(cx, lits, iface, *slot)?,
            _ => return None,
        },
        Callee::Virtual { slot } => {
            let Some(TyKind::Adt(class, _)) = receiver() else {
                return None;
            };
            let mut out = vec![];
            for (i, d) in cx.defs.iter().enumerate() {
                let Some(Def::Adt(a)) = d else { continue };
                if descends(cx, DefId(i as u32), class) {
                    out.extend(a.vtable.get(*slot as usize).map(|&m| (m, 0)));
                }
            }
            out
        }
        _ => return None,
    };
    (!out.is_empty()).then_some(out)
}

/// The implementations of method `slot` of interface `iface`: a class's (or struct's) method,
/// or the functions an object type's literals store in the field of that name.
fn implementations(cx: &Ctx, lits: &Literals, iface: DefId, slot: u32) -> Option<Vec<Target>> {
    if lits.from_any.contains(&iface) {
        return None;
    }
    let name = &cx.iface(iface)?.methods.get(slot as usize)?.name;
    let mut out = vec![];
    for i in cx.impls.iter().filter(|i| i.iface == iface) {
        let TyKind::Adt(d, _) = cx.ty.kind(i.ty) else {
            return None;
        };
        if !object_type(cx.adt(*d)?) {
            out.push((*i.methods.get(slot as usize)?, 0));
            continue;
        }
        out.extend(literal_fns(cx, lits, *d, name)?);
    }
    Some(out)
}

/// The functions the literals of object type `d` store in field `name`; `None` when they are
/// not known completely.
fn literal_fns(cx: &Ctx, lits: &Literals, d: DefId, name: &str) -> Option<Vec<Target>> {
    if lits.spread.contains(&d) {
        return None;
    }
    let k = cx.adt(d)?.fields.iter().position(|f| f.name == name)? as u32;
    let fns = lits.fields.get(&(d, k))?.as_ref()?;
    Some(fns.iter().map(|&g| (g, 1)).collect())
}

/// For a method `m` of an object type (the one sema makes to call the function in the field
/// of its name, which a call through an interface with one implementation calls directly):
/// `Some` of the functions it runs, `None` inside when they are not known completely.
pub(super) fn forwarded(cx: &Ctx, lits: &Literals, m: DefId) -> Option<Option<Vec<Target>>> {
    let f = cx.try_fn(m)?;
    let TyKind::Adt(d, _) = cx.ty.kind(f.this.as_ref()?.ty) else {
        return None;
    };
    if !object_type(cx.adt(*d)?) {
        return None;
    }
    let name = f.name.rsplit('.').next()?;
    Some(literal_fns(cx, lits, *d, name).filter(|t| !t.is_empty()))
}

/// Is `a` an object type (a literal's, `{ render(c) { … } }`, also one sema makes for an
/// interface it is written as), whose methods are the functions in its fields? A class or a
/// declared `struct` has methods of its own.
fn object_type(a: &crate::defs::AdtInfo) -> bool {
    a.kind == AdtKind::Anon || (a.kind == AdtKind::Struct && a.decl.is_none())
}

/// Is class `d` class `base` or one of its subclasses?
fn descends(cx: &Ctx, d: DefId, base: DefId) -> bool {
    let mut d = d;
    for _ in 0..64 {
        if d == base {
            return true;
        }
        let Some(TyKind::Adt(b, _)) = cx
            .adt(d)
            .and_then(|a| a.base)
            .map(|b| cx.ty.kind(b).clone())
        else {
            return false;
        };
        d = b;
    }
    false
}
