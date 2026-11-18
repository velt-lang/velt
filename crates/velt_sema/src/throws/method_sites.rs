//! Interface methods a program writes as properties (`crate::iface_method_writes`) in the
//! inference of function types' error types ([`super::fn_types`]): such a method without a
//! `throws` clause is a site, and so is an object literal's function property of its name.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// Method `name` of an interface is written as a property (`crate::iface_method_writes`):
    /// remembered for the next check, which then makes object literals' function properties of
    /// that name sites. A name new to this check asks for another one.
    pub(crate) fn note_written_method(&mut self, name: &str) {
        if self.fn_sites.seeds.methods.contains(name) {
            return;
        }
        if self.fn_sites.found.methods.insert(name.to_string()) {
            self.fn_sites.grew = true;
            self.fn_sites.sites_grew = true;
        }
    }

    /// Is `name` a method the previous check found written as a property (an object literal's
    /// function property of that name is a site)?
    pub(crate) fn written_method_name(&self, name: &str) -> bool {
        self.fn_sites.seeds.methods.contains(name)
    }

    /// The type a function of type `fn_ty` written through `any` under a written method's name
    /// is stored as (`body/expr/dynamic_write.rs`): its parameters and result, throwing (or, a
    /// promise's, rejecting with) what the method's site `site` holds. The fields storing the
    /// method have that type, which the write checks at run time; `fn_ty` itself when the site
    /// holds nothing yet (a later check, with the value's errors added, has it).
    pub(crate) fn any_method_write_ty(&mut self, fn_ty: TyId, site: Span) -> TyId {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.ty.kind(fn_ty).clone()
        else {
            return fn_ty;
        };
        let Some(members) = self.fn_sites.seeds.sites.get(&site).cloned() else {
            return fn_ty;
        };
        let ms: Vec<TyId> = members.iter().filter_map(|p| self.unport(p)).collect();
        let Some(err) = self.error_union(&ms) else {
            return fn_ty;
        };
        let never = self.ty.never;
        let (ret, throws) = match self.rejection_error(ret) {
            Some(_) if throws == never => (self.with_rejection_error(ret, err), never),
            Some(_) => return fn_ty,
            None => (ret, err),
        };
        self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        })
    }

    /// What a promise of type `t` rejects with (`None`: not a promise).
    pub(crate) fn promise_rejection_error(&self, t: TyId) -> Option<TyId> {
        self.rejection_error(t)
    }

    /// `t` with a promise's rejection error left out (`never`): how a stored function's result
    /// is compared with a written method's (`Ctx::stores_method`), whose error types are
    /// inferred, never compared.
    pub(crate) fn without_rejection_error(&mut self, t: TyId) -> TyId {
        match self.rejection_error(t) {
            Some(_) => {
                let never = self.ty.never;
                self.with_rejection_error(t, never)
            }
            None => t,
        }
    }

    /// The type of an object literal's function value `t`, stored at `span` in module `module`
    /// under the name of a method the program writes as a property: a site, as
    /// [`Self::untyped_let_site`] makes one, also when the value throws (or its promise
    /// rejects with) errors of its own. The field holds every function assigned to the method,
    /// so what it throws is one set: the value's own errors join it here, and what the writes
    /// assign joins it through the links to the method's site (`Ctx::link_method_sites`).
    /// `None` when it can't be a site (outside a site module, or a function returning a promise
    /// that also throws): the field's own type then has to allow what is assigned.
    pub(crate) fn method_value_site(&mut self, t: TyId, span: Span, module: usize) -> Option<TyId> {
        let TyKind::FnPtr {
            params,
            mut ret,
            throws,
            rest,
        } = self.ty.kind(t).clone()
        else {
            return None;
        };
        if !self.site_module(module) || span == Span::DUMMY {
            return None;
        }
        let never = self.ty.never;
        let own = match self.rejection_error(ret) {
            Some(_) if throws != never => return None,
            Some(e) => e,
            None => throws,
        };
        if own != never && own != self.ty.error {
            let mut members = vec![];
            for m in self.error_members(own) {
                members.push(self.port_thrown(m)?);
            }
            self.add_to_site(span, &members);
        }
        let Some(err) = self.site_error(span, module) else {
            self.record_fn_site(span, module, t);
            return Some(t);
        };
        let throws = match self.rejection_error(ret) {
            Some(_) => {
                ret = self.with_rejection_error(ret, err);
                never
            }
            None => err,
        };
        let s = self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        });
        self.record_fn_site(span, module, s);
        self.ty.inferred_fns.insert(s);
        Some(s)
    }

    /// The function type `(params) => ret` of an interface method without a `throws` clause that
    /// the program writes as a property (`crate::iface_method_writes`), as a site at `span` in
    /// module `module`: what it throws is inferred from what the program assigns to it and what
    /// the fields storing it throw, as for a function type written without `throws`
    /// (`Ctx::resolve_fn_type`). `None` outside a site module.
    pub(crate) fn method_site_ty(
        &mut self,
        params: Vec<TyId>,
        ret: TyId,
        span: Span,
        module: usize,
    ) -> Option<TyId> {
        if !self.site_module(module) || span == Span::DUMMY {
            return None;
        }
        let never = self.ty.never;
        let var = self.fn_sites.user_vars;
        let err = match var {
            true => self.site_var(span, module),
            false => self.site_error(span, module),
        };
        // A method returning a promise rejects it with what is assigned to it, as a function
        // type written without `throws` does (`Ctx::untyped_let_site`).
        let (site_ret, throws) = match (err, self.rejection_error(ret)) {
            (Some(e), Some(_)) => (self.with_rejection_error(ret, e), never),
            (e, _) => (ret, e.unwrap_or(never)),
        };
        let t = self.ty.intern(TyKind::FnPtr {
            params: params.clone(),
            ret: site_ret,
            throws,
            rest: false,
        });
        self.record_fn_site(span, module, t);
        match (var, err) {
            (true, Some(_)) => {
                let plain = self.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws: never,
                    rest: false,
                });
                self.fn_sites.var_sets.note_written(t, plain);
            }
            (false, Some(_)) => {
                self.ty.inferred_fns.insert(t);
            }
            _ => {}
        }
        Some(t)
    }
}
