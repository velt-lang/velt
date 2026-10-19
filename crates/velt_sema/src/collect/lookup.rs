//! Method lookup on a struct / class: its own methods, then (classes) each base class in turn;
//! at each level, default methods of the interfaces that level implements count as its own.

use crate::ctx::Ctx;
use crate::defs::MethodRef;
use crate::hir::{DefId, TyId, TyKind};

/// Where a method of a class comes from.
#[derive(Clone, Debug)]
pub(crate) enum Found {
    /// Declared by class `owner` (the receiver's class or an ancestor), instantiated with `args`.
    Class {
        m: MethodRef,
        owner: DefId,
        args: Vec<TyId>,
    },
    /// Default body of interface `iface` implemented by `implementor` (the receiver's class or an
    /// ancestor type).
    Default {
        def: DefId,
        iface: DefId,
        iface_args: Vec<TyId>,
        implementor: TyId,
    },
}

impl Found {
    pub fn def(&self) -> DefId {
        match self {
            Found::Class { m, .. } => m.def,
            Found::Default { def, .. } => *def,
        }
    }

    pub fn is_static(&self) -> bool {
        matches!(self, Found::Class { m, .. } if m.is_static)
    }

    /// Type args of the owner's generics (the prefix of the method def's generics).
    pub fn owner_args(&self) -> Vec<TyId> {
        match self {
            Found::Class { args, .. } => args.clone(),
            Found::Default {
                iface_args,
                implementor,
                ..
            } => {
                let mut v = iface_args.clone();
                v.push(*implementor);
                v
            }
        }
    }

    /// The type the receiver is converted to (the declaring class / implementing type).
    pub fn recv_ty(&self, cx: &mut Ctx) -> TyId {
        match self {
            Found::Class { owner, args, .. } => cx.ty.intern(TyKind::Adt(*owner, args.clone())),
            Found::Default { implementor, .. } => *implementor,
        }
    }
}

/// Method `name` of struct/class `d` instantiated with `args`. Statics and instance members are
/// separate namespaces, as in TypeScript: a static method of a subclass does not hide an
/// inherited instance method of the same name. A static method is the answer only when no
/// instance method has the name (so the caller can say it is static).
pub(crate) fn lookup_method(cx: &mut Ctx, d: DefId, args: &[TyId], name: &str) -> Option<Found> {
    let (mut d, mut args) = (d, args.to_vec());
    let mut first_static = None;
    for _ in 0..64 {
        let Some(a) = cx.adt(d) else { break };
        if let Some(m) = a.methods.get(name) {
            let found = Found::Class {
                m: *m,
                owner: d,
                args: args.clone(),
            };
            if !m.is_static {
                return Some(found);
            }
            first_static.get_or_insert(found);
        }
        let (implements, base) = (a.implements.clone(), a.base);
        for b in implements {
            let Some(i) = cx.iface(b.iface) else { continue };
            let Some(im) = i.methods.iter().find(|m| m.name == name) else {
                continue;
            };
            let Some(def) = im.default else {
                continue;
            };
            let iface_args = b.args.iter().map(|t| cx.subst(*t, &args)).collect();
            let implementor = cx.ty.intern(TyKind::Adt(d, args.clone()));
            return Some(Found::Default {
                def,
                iface: b.iface,
                iface_args,
                implementor,
            });
        }
        let Some(base) = base else { break };
        let base = cx.subst(base, &args);
        let Some(next) = cx.class_of(base) else { break };
        (d, args) = next;
    }
    first_static
}
