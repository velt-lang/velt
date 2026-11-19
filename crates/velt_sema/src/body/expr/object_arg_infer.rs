//! Type arguments inferred from an object literal's function members, as TypeScript infers
//! them: `run({ handler: (rq: Ctx, n: number) => n * 2 })` for
//! `run<S, R>(o: Options<S, R>)` with `handler(rq: Ctx, input: S): R | Promise<R>` gives
//! `S = number` and `R = number`. A member arrow whose parameters are all written is checked on
//! its own (a trial, rolled back), and its type offers candidates as an arrow argument's does;
//! the literal is then checked against the interface at those arguments.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Candidates for `slots` from object literal `arg` where `pat` (an interface over the call's
    /// type parameters, or one or nothing) is expected.
    pub(super) fn infer_from_object_members(
        &mut self,
        arg: &ast::Expr,
        pat: TyId,
        slots: &mut [Option<TyId>],
    ) {
        let ast::ExprKind::Object(props) = &arg.kind else {
            return;
        };
        let core = self.cx.ty.opt_payload(pat).unwrap_or(pat);
        let TyKind::Dyn(d, args) = self.cx.ty.kind(core).clone() else {
            return;
        };
        if self.cx.dynamic(core).is_some() || slots.iter().all(Option::is_some) {
            return;
        }
        let Some(info) = self.cx.iface(d) else {
            return;
        };
        let methods: Vec<(String, Vec<TyId>, TyId)> = (info.methods.iter())
            .filter(|m| m.generics.names.is_empty() && !m.is_getter)
            .map(|m| {
                (
                    m.name.clone(),
                    m.params.iter().map(|p| p.ty).collect(),
                    m.ret,
                )
            })
            .collect();
        let mut sargs = args.clone();
        sargs.push(self.cx.ty.error);
        for prop in props {
            let ast::ObjectProp::KeyValue(key, value) = prop else {
                continue;
            };
            let ast::ExprKind::Arrow {
                type_params,
                params,
                ..
            } = &value.kind
            else {
                continue;
            };
            // An arrow without parameters says nothing by what it takes, and its result is
            // better typed by the call's context (`const n: i32 = pick({ get: () => 1 })`).
            if !type_params.is_empty() || params.is_empty() || params.iter().any(|p| p.ty.is_none())
            {
                continue;
            }
            let Some((_, ps, ret)) = methods.iter().find(|(n, ..)| *n == key.name) else {
                continue;
            };
            let params: Vec<TyId> = ps.iter().map(|t| self.cx.subst(*t, &sargs)).collect();
            let ret = self.cx.subst(*ret, &sargs);
            let unknown = self.cx.ty.error;
            let want = self.cx.ty.intern(TyKind::FnPtr {
                params,
                ret,
                throws: unknown,
                rest: false,
            });
            // A method that names none of the call's type parameters has nothing to offer.
            if !super::iface_adapt::has_params(self.cx, want) {
                continue;
            }
            // Its own type, without what the call expects of it.
            let Some(have) = self.remembered_type(value) else {
                continue;
            };
            self.cx.match_ty(want, have, slots);
        }
    }
}
