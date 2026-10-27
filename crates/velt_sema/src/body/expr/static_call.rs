//! Calls of static methods and namespace members (`C.m(…)`, `ns.f(…)`), split from call.rs:
//! finding the static (own, inherited or from an `extend` block) and the owner's type
//! arguments it takes.

use velt_common::Span;
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, Callee, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Method `name` that an `extend` block of class/struct `d` adds (a static one is called as
    /// `C.make()`), with the number of the block's own type parameters.
    pub(super) fn extension_static(
        &self,
        d: DefId,
        name: &str,
    ) -> Option<(crate::defs::MethodRef, usize)> {
        self.cx.extensions.iter().find_map(|x| {
            let targets_d = matches!(self.cx.ty.kind(x.target), TyKind::Adt(t, _) if *t == d);
            let m = x.methods.get(name).filter(|_| targets_d)?;
            Some((*m, x.generics.len()))
        })
    }

    /// `Type.member(...)`: static methods, enum variants, builtin namespaces.
    pub(super) fn namespace_call(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if let (ast::ExprKind::This, Some((this, declaring))) = (&object.kind, self.static_this) {
            // `this.f()` in a static method: the class it was called on.
            return Some(self.this_static_call(this, declaring, prop, type_args, args, exp, span));
        }
        if let Some(h) = self.primitive_namespace_call(object, prop, type_args, args, exp, span) {
            return Some(h);
        }
        let ast::ExprKind::Ident(id) = &object.kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let item = self.lookup_item(&id.name, id.span);
        if let Some(simd) = self.simd_type_named(&id.name, item) {
            return self.simd_static_call(simd, id, prop, type_args, args, exp, span);
        }
        match item {
            Some(Item::Def(d)) if self.cx.enum_info(d).is_some() => {
                Some(self.variant_value(d, prop, args, exp, span))
            }
            Some(Item::Def(d)) if self.cx.adt(d).is_some() => {
                Some(self.static_call(d, prop, type_args, args, exp, span))
            }
            Some(item) => {
                let d = self.companion_class(&id.name, item)?;
                Some(self.static_call(d, prop, type_args, args, exp, span))
            }
            None if (id.name.as_str(), prop.name.as_str()) == ("Promise", "withResolvers") => {
                Some(self.promise_with_resolvers(type_args, args, exp, span))
            }
            None if id.name == "Promise" && matches!(prop.name.as_str(), "resolve" | "reject") => {
                Some(self.promise_settled(&prop.name, type_args, args, exp, span))
            }
            None => self.namespace_builtin(id, prop, args, exp, span),
        }
    }

    pub(super) fn static_call(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        self.static_call_as(d, d, prop, type_args, args, exp, span)
    }

    /// `C.f(args)`, the static method `f` that class `d` declares or inherits, run with `this`
    /// being class `this_class` (`d` or a subclass of it: `this.f()` and `super.f()` in a static
    /// method keep the class the outer call was made on).
    #[allow(clippy::too_many_arguments)] // `static_call` plus the class `this` is
    pub(super) fn static_call_as(
        &mut self,
        d: DefId,
        this_class: DefId,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        if self.cx.has_overloads {
            let h = self.overloaded_static_call(d, this_class, prop, type_args, args, exp, span);
            if let Some(h) = h {
                return h;
            }
        }
        self.note_reflect_call(d, &prop.name, span);
        let cname = self.cx.adt(d).expect("ICE: adt").name.clone();
        // `JSON.parse(text)` with its type from the context (`as T`, an annotated variable):
        // TypeScript's form, decoded strictly where `JSON.parse<T>` is lenient (std's
        // `JSON.__parseFromContext`).
        let from_context = type_args.is_empty()
            && prop.name == "parse"
            && cname == "JSON"
            && self.cx.declared_in_std(d);
        let lookup = match from_context {
            true => "__parseFromContext",
            false => prop.name.as_str(),
        };
        let (m, owner_generics, owner_args) = match self.find_static(d, lookup) {
            Ok(found) => found,
            // A static field holding a function value: a call of what it holds.
            Err(false) if self.static_fn_field(d, prop) => {
                return self.static_field_call(d, prop, args, span);
            }
            Err(instance) => {
                let what = match instance {
                    true => "an instance method",
                    false => "not a static method",
                };
                self.cx
                    .err(format!("`{cname}.{}` is {what}", prop.name), prop.span);
                self.check_args_loose(args);
                return self.error_expr(span);
            }
        };
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Def(m.def));
        let private_to = self.fn_private_to(m.def);
        self.check_private(private_to, &prop.name, prop.span);
        let def = self.static_def_for(m.def, this_class);
        let mut c = self.fn_callable(def, format!("`{cname}.{}`", prop.name), span);
        let n = c.slot_names.len();
        let own = n - owner_generics;
        if self.declares_static(d, &prop.name, m.def) {
            self.owner_defaults(&mut c, d, owner_generics);
        }
        self.default_unused_owner_slots(&mut c, owner_generics);
        let mut slots = vec![None; n];
        for (slot, a) in slots.iter_mut().zip(owner_args.into_iter().flatten()) {
            *slot = Some(a);
        }
        self.explicit_type_args(&mut slots, own, type_args, &c.defaults, span);
        let mut ck = self.check_call(&c, slots, args, exp, span);
        if let Some(h) = self.sync_call(def, &mut ck.args, ck.ret, span) {
            return h;
        }
        self.note_async_args(def, &ck.args);
        self.call_throws(def, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(def, ck.type_args),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }

    /// A static method of class `d` (whose `owner` first slots are `d`'s type parameters) called
    /// with slots nothing infers: they take `d`'s defaults, as a `new` does.
    pub(super) fn owner_defaults(&mut self, c: &mut Callable, d: DefId, owner: usize) {
        let defaults = self.cx.adt_param_defaults(d);
        if defaults.len() != owner || defaults.iter().all(Option::is_none) {
            return;
        }
        c.defaults.resize(c.slot_names.len(), None);
        c.defaults[..owner].copy_from_slice(&defaults);
    }

    /// Does class `d` itself declare the static method `name` as `m` (not inherit it or take it
    /// from an `extend` block)?
    pub(super) fn declares_static(&self, d: DefId, name: &str, m: DefId) -> bool {
        let a = self.cx.adt(d).expect("ICE: adt");
        a.methods
            .get(&crate::defs::static_key(name))
            .or_else(|| a.methods.get(name))
            .is_some_and(|x| x.is_static && x.def == m)
    }

    /// The static method `name` of class `d`: its own (in the class, then in an `extend`
    /// block), else the nearest base class's. With the number of generics of the class (or
    /// block) declaring it and, when known, their type arguments. `Err(true)`: `name` is an
    /// instance method.
    pub(super) fn find_static(
        &mut self,
        d: DefId,
        name: &str,
    ) -> Result<(crate::defs::MethodRef, usize, Option<Vec<TyId>>), bool> {
        let a = self.cx.adt(d).expect("ICE: adt");
        let mut owner_generics = a.generics.len();
        let mut m = a
            .methods
            .get(&crate::defs::static_key(name))
            .or_else(|| a.methods.get(name))
            .copied();
        let mut owner_args = None;
        if m.is_none() {
            if let Some((em, n)) = self.extension_static(d, name) {
                (m, owner_generics) = (Some(em), n);
            }
        }
        if !m.is_some_and(|m| m.is_static) {
            // Statics are inherited (TypeScript): `B.f()` calls base class `A`'s `static f`,
            // unless `B` has its own `static f` (in the class or an `extend B` block).
            if let Some((im, n, args)) = self.inherited_static(d, name) {
                (m, owner_generics, owner_args) = (Some(im), n, args);
            }
        }
        match m {
            Some(m) if m.is_static => Ok((m, owner_generics, owner_args)),
            found => Err(found.is_some()),
        }
    }

    /// The function running static method `m` with `this` being class `this_class`: `m`, or its
    /// copy for that subclass when `m` uses `this` (`collect::static_this`).
    /// A copy used is recorded (`Ctx::copy_uses`).
    pub(crate) fn static_def_for(&mut self, m: DefId, this_class: DefId) -> DefId {
        match self.cx.static_this.get(&m) {
            Some(&declaring) if declaring != this_class => {
                match self.cx.static_copies.get(&(m, this_class)).copied() {
                    Some(copy) => {
                        self.cx.copy_uses.push((self.body_def, copy));
                        copy
                    }
                    None => m,
                }
            }
            _ => m,
        }
    }

    /// The class's type parameters that a static method's signature doesn't use (always the
    /// case in TypeScript, where statics can't use them) default to `void`, so `Box.wrap(4)`
    /// needs no type arguments, bounded (`class Box<T extends Named>`) or not: nothing of the
    /// call depends on it, so its bound isn't checked.
    pub(super) fn default_unused_owner_slots(&mut self, c: &mut Callable, owner_generics: usize) {
        let tys: Vec<TyId> = c.params.iter().map(|p| p.ty).chain([c.ret]).collect();
        let unused = self.unused_owner_slots(&tys, owner_generics);
        let n = c.slot_names.len();
        for i in unused {
            c.defaults.resize(n, None);
            c.defaults[i].get_or_insert(self.cx.ty.unit);
            if let Some(b) = c.bounds.get_mut(i) {
                b.clear();
            }
        }
    }

    /// The slots below `owner_generics` that no type of `tys` uses.
    pub(super) fn unused_owner_slots(&self, tys: &[TyId], owner_generics: usize) -> Vec<usize> {
        let mut used = vec![];
        for &t in tys {
            crate::types::collect_params(&self.cx.ty, t, &mut used);
        }
        (0..owner_generics)
            .filter(|&i| !used.contains(&(i as u32)))
            .collect()
    }

    /// The nearest base class of class `d` declaring a static method `name`: that method, the
    /// number of generics of the class declaring it, and that class's type arguments in the
    /// `extends` chain from `d` (when `d` is not generic, so they are known).
    pub(super) fn inherited_static(
        &mut self,
        d: DefId,
        name: &str,
    ) -> Option<(crate::defs::MethodRef, usize, Option<Vec<TyId>>)> {
        let key = crate::defs::static_key(name);
        let mut cur = d;
        let mut args = (self.cx.adt(d)?.generics.len() == 0).then(Vec::new);
        for _ in 0..64 {
            let base = self.cx.adt(cur)?.base?;
            let base = match &args {
                Some(a) => self.cx.subst(base, a),
                None => base,
            };
            let (c, base_args) = self.cx.class_of(base)?;
            cur = c;
            args = args.map(|_| base_args);
            let a = self.cx.adt(cur)?;
            let m = a.methods.get(&key).or_else(|| a.methods.get(name));
            if let Some(m) = m.filter(|m| m.is_static) {
                return Some((*m, a.generics.len(), args));
            }
            // Then the statics an `extend` block of that base adds.
            let n = a.generics.len();
            if let Some((m, k)) = self
                .extension_static(cur, name)
                .filter(|(m, _)| m.is_static)
            {
                return Some((m, k, args.filter(|_| k == n)));
            }
        }
        None
    }
}
