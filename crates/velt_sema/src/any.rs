//! `any` and `unknown`, the dynamic value (issue #924, design note on #823; hir_encodings.md
//! "any and unknown").
//!
//! Both are interfaces of the prelude (`std/prelude/z_dynamic.vlt`) that every type implements
//! through one generic implementation sema adds here (`Program::any_impl`, `impl<T> any for
//! T` with no methods). A value converted to either is an interface value (`ToDyn`): the
//! value, boxed unless it is a class instance or a counted object, and the interface table
//! of its concrete type, which is its type descriptor. Both types share it, so converting
//! between them changes nothing at run time. `T | null` and unions are unpacked when they are
//! boxed: a dynamic value always holds one concrete type, and `null` / `undefined` hold the
//! nothing of `boolean | null` / `boolean | undefined`.

use crate::ctx::Ctx;
use crate::hir::{DefId, ImplDef, Nothing, TyId, TyKind};

/// The prelude's dynamic interfaces and their implementation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnyDefs {
    pub any: DefId,
    pub unknown: DefId,
    /// Index of `impl<T> any for T` in `Ctx::impls`, added when a value is first converted
    /// to `any` or `unknown` ([`Ctx::any_impl`]): a program without them is unchanged.
    pub imp: Option<u32>,
}

/// Which dynamic type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dynamic {
    Any,
    Unknown,
}

/// After the declared implementations: find the prelude's dynamic interfaces.
pub(crate) fn add_impl(cx: &mut Ctx) {
    let (Some(any), Some(unknown)) = (cx.prelude_iface("any"), cx.prelude_iface("unknown")) else {
        return;
    };
    cx.any = Some(AnyDefs {
        any,
        unknown,
        imp: None,
    });
}

impl Ctx<'_> {
    /// The index of `impl<T> any for T`, added on first use.
    pub(crate) fn any_impl(&mut self) -> Option<u32> {
        let defs = self.any?;
        if let Some(i) = defs.imp {
            return Some(i);
        }
        let t = self.ty.intern(TyKind::Param(0));
        self.impls.push(ImplDef {
            ty: t,
            generics: 1,
            iface: defs.any,
            iface_args: vec![],
            methods: vec![],
        });
        let i = self.impls.len() as u32 - 1;
        self.any = Some(AnyDefs {
            imp: Some(i),
            ..defs
        });
        Some(i)
    }

    /// Is `t` `any` or `unknown`?
    pub(crate) fn dynamic(&self, t: TyId) -> Option<Dynamic> {
        let defs = self.any?;
        match self.ty.kind(t) {
            TyKind::Dyn(d, _) if *d == defs.any => Some(Dynamic::Any),
            TyKind::Dyn(d, _) if *d == defs.unknown => Some(Dynamic::Unknown),
            _ => None,
        }
    }

    /// Is `t` `any` or `unknown`, or one of them or `null` (`any | null`)?
    pub(crate) fn maybe_dynamic(&self, t: TyId) -> bool {
        let t = self.ty.opt_payload(t).unwrap_or(t);
        self.dynamic(t).is_some()
    }

    /// The type of the value a dynamic `null` (`Nothing::Null`) or `undefined` holds:
    /// `boolean | null` / `boolean | undefined`, whose nothing it is.
    pub(crate) fn dynamic_nothing_ty(&mut self, n: Nothing) -> TyId {
        let b = self.ty.bool_;
        let n = if n.has_null() {
            Nothing::Null
        } else {
            Nothing::Undefined
        };
        self.ty.intern(TyKind::Option(b, n))
    }
}
