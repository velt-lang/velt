//! The fields a new value may get a captured value in, for [`super::reach`] (#854). A part of
//! a new value (`ctx.app` of `new Ctx(app)`) may be the captured value only when something
//! stores a value that is not new in that field: a constructor's `this.app = app`, an
//! assignment `ctx.app = …`, a literal's `{ app }`, an initializer `out = this.app.list`, or a
//! spread or conversion copying fields. A field only ever filled with new values (`out = []`,
//! `seen = new Map()`, `{ buf: [] }`) is the request's own, so a change to it changes no
//! captured value. A field is known by the type that declares it and its position, so a store
//! through a subclass counts for the base class's field.

use rustc_hash::FxHashSet;

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::ownership::local_closures::walk::{self, Visit};

use super::changes::Memo;
use super::reach::{fresh, plain};

/// A field: the type that declares it and its index there.
pub(super) type Field = (DefId, u32);

/// The fields some code stores a value that is not new in (module docs); `None` when the
/// program stores into a field of a type not followed here (a type parameter), which may be any.
pub(super) fn fed_fields(cx: &mut Ctx, keyed: FxHashSet<Field>) -> Option<FxHashSet<Field>> {
    // Fields fed by key (`self.out = v` through an `any`, a helper's `o[k] = v`): super::keyed.
    let mut fed = keyed;
    let mut memo = Memo::default();
    for i in 0..cx.defs.len() {
        // Asked before the definition is taken out of `cx.defs`.
        let library = super::changes::in_library(cx, DefId(i as u32));
        let Some(def) = cx.defs[i].take() else {
            continue;
        };
        let mut stores = vec![];
        match &def {
            Def::Fn(f) => walk::block(&f.body.block, &mut Stores(&mut stores)),
            Def::Global(g) => walk::expr(&g.init, &mut Stores(&mut stores)),
            Def::Adt(a) => {
                let d = DefId(i as u32);
                for (k, f) in a.fields.iter().enumerate() {
                    if let Some(v) = &f.default {
                        stores.push(Store::Field((d, k as u32), v.clone()));
                        walk::expr(v, &mut Stores(&mut stores));
                    }
                }
                for init in &a.inherited_inits {
                    stores.push(Store::Field((d, init.field), init.init.clone()));
                    walk::expr(&init.init, &mut Stores(&mut stores));
                }
            }
            _ => {}
        }
        cx.defs[i] = Some(def);
        for s in stores {
            let (field, value) = match s {
                Store::Field((d, k), value) => match declaring(cx, d, k) {
                    Some(f) => (f, value),
                    None => continue,
                },
                Store::Named(ty, k, value) => match field_of(cx, ty, k) {
                    Some(f) => (f, value),
                    // A store into a field of a type parameter: any field may be fed. Library
                    // code is left out, as it stores into the program's types only through
                    // values it was given.
                    None if !library => return None,
                    None => continue,
                },
                Store::Every(ty) => {
                    let n = field_count(cx, ty);
                    fed.extend((0..n).filter_map(|k| field_of(cx, ty, k)));
                    continue;
                }
            };
            if !fed.contains(&field) && !own(cx, &value, &mut memo) {
                fed.insert(field);
            }
        }
    }
    Some(fed)
}

/// Is a value stored by `value` the request's own: one that holds no reference, or a new one?
pub(super) fn own(cx: &mut Ctx, value: &Expr, memo: &mut Memo) -> bool {
    plain(cx, value.ty) || matches!(value.kind, E::Lit(_) | E::Closure(_)) || fresh(cx, value, memo)
}

/// The ADT a value of `t` is (seen through an option).
pub(super) fn adt_of(cx: &Ctx, t: TyId) -> Option<DefId> {
    let t = match cx.ty.kind(t) {
        TyKind::Option(inner, _) => *inner,
        _ => t,
    };
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => Some(*d),
        _ => None,
    }
}

fn field_count(cx: &Ctx, t: TyId) -> u32 {
    adt_of(cx, t)
        .and_then(|d| cx.adt(d))
        .map_or(0, |a| a.fields.len() as u32)
}

/// Field `k` of a value of type `t`, as the type declaring it knows it.
pub(super) fn field_of(cx: &Ctx, t: TyId, k: u32) -> Option<Field> {
    declaring(cx, adt_of(cx, t)?, k)
}

/// Field `k` of ADT `d`, as the class declaring it knows it: a base class's fields are a
/// prefix of its subclasses'.
pub(super) fn declaring(cx: &Ctx, d: DefId, k: u32) -> Option<Field> {
    let mut d = d;
    if k as usize >= cx.adt(d)?.fields.len() {
        return None;
    }
    for _ in 0..64 {
        let Some(TyKind::Adt(b, _)) = cx.adt(d)?.base.map(|b| cx.ty.kind(b).clone()) else {
            break;
        };
        if (k as usize) < cx.adt(b)?.fields.len() {
            d = b;
        } else {
            break;
        }
    }
    Some((d, k))
}

/// What a body stores in fields: a value in a field of an ADT, or in field `index` of a value
/// of a type, or every field of a type (a spread copies them all).
enum Store {
    Field(Field, Expr),
    Named(TyId, u32, Expr),
    Every(TyId),
}

struct Stores<'a>(&'a mut Vec<Store>);

impl Visit for Stores<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                if let E::Field { base, index, .. } = &place.kind {
                    self.0
                        .push(Store::Named(base.ty, *index, (**value).clone()));
                }
            }
            E::AdtLit { fields, .. } => {
                for (i, v) in fields.iter().enumerate() {
                    self.0.push(Store::Named(e.ty, i as u32, v.clone()));
                }
            }
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::SpreadObject),
                ..
            } => self.0.push(Store::Every(e.ty)),
            _ => {}
        }
    }
}
