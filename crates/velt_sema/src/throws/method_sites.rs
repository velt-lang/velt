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
        let t = self.ty.intern(TyKind::FnPtr {
            params: params.clone(),
            ret,
            throws: err.unwrap_or(never),
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
