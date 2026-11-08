//! `typeof f` of a generic or overloaded function (#2617, #936), as TypeScript types it: the
//! function's own signature, which stays generic, or the set of its overload signatures.
//!
//! - A generic function `f<T>(x: T): R` gives the generic function type `<T>(x: T) => R`
//!   (`crate::generic_fns`): a field, variable or parameter of that type holds a value of it,
//!   and each call through it instantiates `T` (from the arguments, or written: `g<number>(1)`).
//! - A function with TypeScript-form overloads (`crate::overloads`) gives the set of its
//!   signatures (the implementation's own is not part of it, as in `tsc`). Velt holds such a
//!   value as an object type of one field per signature, named `#0`, `#1`, … (names no source
//!   can spell), each of that signature's function type: [`Ctx::overload_set_parts`]. A call
//!   through the value tries the fields in order and calls the first that accepts the
//!   arguments, as a call of `f` itself does (`body::expr::overload_sets`); `f` used where the
//!   set is expected is the object of its signatures' values.
//!
//! The type comes from the declaration alone, so the signature needs a written result type,
//! and each parameter a plain one (no optional or default parameters); for any other form
//! `None`, and the caller keeps its error.

use velt_common::Span;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnSource};
use crate::hir::{AdtKind, DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// The name of field `k` of an overload set's object type.
pub(crate) fn set_field(k: usize) -> String {
    format!("#{k}")
}

/// Is `name` the name of a field of an overload set's object type?
pub(crate) fn is_set_field(name: &str) -> bool {
    name.strip_prefix('#')
        .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
}

/// Is `k` a key the compiler wrote into an object literal (an empty span): a callable
/// object's hidden function (`crate::callables`) or an overload set's field?
pub(crate) fn compiler_key(k: &velt_syntax::ast::Ident) -> bool {
    let hidden = crate::callables::is_hidden_field(&k.name) || is_set_field(&k.name);
    hidden && k.span.lo == k.span.hi
}

impl Ctx<'_> {
    /// The type `typeof f` gives function `d` when it is generic or overloaded (module docs),
    /// `None` when it is neither, or its declaration doesn't give one.
    pub(crate) fn generic_query_type(&mut self, d: DefId, span: Span) -> Option<TyId> {
        let sigs = self.overload_sig_defs(d);
        if sigs.is_empty() {
            // From the declaration: before collection is finished its signature may not be
            // resolved yet (an interface's field).
            let generic = match self.fn_info(d).source {
                Some(FnSource::Decl(decl)) => !decl.sig.generics.is_empty(),
                _ => false,
            };
            if !generic {
                return None;
            }
            let t = self.declared_sig_type(d, span)?;
            // Generic only over what its callbacks throw (`setTimeout<E>`): the plain function
            // type (`Ctx::query_fn`).
            let g = self.generic_sig(t)?;
            let tys = g.method.params.iter().map(|p| p.ty).chain([g.method.ret]);
            let errors_only = tys
                .collect::<Vec<_>>()
                .into_iter()
                .all(|t| !self.mentions_beyond_errors(t, 0));
            return (!errors_only).then_some(t);
        }
        let module = self.fn_info(d).module;
        let mut fields = vec![];
        for (k, s) in sigs.into_iter().enumerate() {
            let t = self.declared_sig_type(s, span)?;
            fields.push((set_field(k), t));
        }
        self.overload_sets = true;
        Some(self.anon_type(&fields, module))
    }

    /// The signatures `f#0`, `f#1`, … of overloaded function `d` (empty when it has none).
    pub(crate) fn overload_sig_defs(&self, d: DefId) -> Vec<DefId> {
        let f = self.fn_info(d);
        let name = f.name.rsplit([':', '.']).next().unwrap_or(&f.name);
        let module = f.module;
        let mut sigs = vec![];
        loop {
            let sig = crate::overloads::sig_name(name, sigs.len());
            match self.lookup_item(module, &sig) {
                Some(Item::Def(s)) if matches!(self.info[s.0 as usize], DefInfo::Fn(_)) => {
                    sigs.push(s)
                }
                _ => break,
            }
        }
        sigs
    }

    /// The function type function `d`'s declaration writes, generic when it has type
    /// parameters (module docs).
    fn declared_sig_type(&mut self, d: DefId, span: Span) -> Option<TyId> {
        let f = self.fn_info(d);
        let (module, Some(FnSource::Decl(decl))) = (f.module, f.source) else {
            return None;
        };
        let sig = &decl.sig;
        let plain = sig
            .params
            .iter()
            .all(|p| !p.optional && p.default.is_none());
        if sig.ret.is_none() || sig.is_async || sig.is_generator || sig.this_param.is_some() {
            return None;
        }
        if !plain {
            return None;
        }
        let written = crate::call_sigs::fn_type(sig, span);
        let t = self.resolve_type(&written, &TyEnv::new(module, &[]));
        (t != self.ty.error).then_some(t)
    }

    /// The signatures' function types of overload set type `t` (module docs), in order; `None`
    /// when `t` is not one.
    pub(crate) fn overload_set_parts(&mut self, t: TyId) -> Option<Vec<TyId>> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let a = self.adt(d)?;
        if a.kind != AdtKind::Anon || a.fields.is_empty() {
            return None;
        }
        let named = a
            .fields
            .iter()
            .enumerate()
            .all(|(k, f)| f.name == set_field(k));
        if !named {
            return None;
        }
        let tys: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
        Some(tys.into_iter().map(|t| self.subst(t, &args)).collect())
    }
}
