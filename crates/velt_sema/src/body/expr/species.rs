//! `speciesNew(receiver, args…)` (std/lang/intrinsics/types.velt): a new object of the receiver's
//! own class, as JavaScript's SpeciesConstructor makes the copy a built-in method works on or
//! returns (`s.split(re)` runs on `new C(re, flags)`, #3817), or `null` where std makes the copy
//! itself (of its own class `T`). Written out as a test of the receiver's class, deepest first,
//! over the program's classes that extend its static class `T`:
//!
//! ```text
//! match (receiver) { InstanceOf(C) => new C(args…), …, _ => null }
//! ```
//!
//! A class whose constructor can't take `args` (it takes other parameters than `T`'s, which
//! JavaScript calls with `T`'s arguments anyway) gets `null`, so a copy of `T`, with a warning
//! naming it. A constructor with fewer parameters gets the first arguments, as JavaScript drops
//! the others. With no class extending `T` it is `null`, the receiver not evaluated. A subclass's
//! constructor throws what it does, as in JavaScript; std's own copy is std's to make without
//! throwing, so no program gets what `T`'s constructor declares it throws.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, PatKind as P, TyId};

impl FnCx<'_, '_> {
    pub(super) fn species_new(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let Some((recv, rest)) = args.split_first() else {
            return self.species_error("`speciesNew` needs the receiver first", span, args);
        };
        if let Some(a) = args.iter().find(|a| !plain(a)) {
            let msg = "the receiver and the arguments of `speciesNew` must be local variables or literals";
            return self.species_error(msg, a.span, args);
        }
        let r = self.expr(recv, None, Want::Borrow);
        let rty = r.ty;
        let Some((base, _)) = self.cx.class_of(rty) else {
            let msg = "the receiver of `speciesNew` must be an object of a class";
            return self.species_error(msg, recv.span, &[]);
        };
        let oty = self.cx.ty.option(rty);
        // A library body with a dispatch over the program's classes is the program's glue (also
        // where a library checked on its own sees none: the program's may extend `T`).
        if self.cx.scopes[self.module].is_library {
            if let Some(d) = self.body_def {
                self.cx.glue_bodies.insert(d);
            }
        }
        let subs = self.class_subclasses(rty).unwrap_or_default();
        if subs.is_empty() {
            return self.mk(H::Lit(hir::Lit::Null), oty, span);
        }
        let mut arms = vec![];
        for c in subs {
            let body = match self.species_fits(c, rest, span) {
                true => self.species_of(c, rest, oty, span),
                false => {
                    self.species_warning(c, base, rest);
                    self.mk(H::Lit(hir::Lit::Null), oty, span)
                }
            };
            arms.push(hir::Arm {
                pat: self.pat(P::InstanceOf(c), rty, span),
                guard: None,
                body,
            });
        }
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, rty, span),
            guard: None,
            body: self.mk(H::Lit(hir::Lit::Null), oty, span),
        });
        let kind = H::Match {
            scrutinee: Box::new(r),
            arms,
        };
        self.mk(kind, oty, span)
    }

    /// `new c(args…)` as a `ty` (`T | null`): `args` cut to as many as `c`'s constructor takes.
    fn species_of(&mut self, c: DefId, args: &[ast::Expr], ty: TyId, span: Span) -> hir::Expr {
        let args = &args[..self.species_arity(c, args.len())];
        let h = self.new_class(c, vec![], (span, None), args, None, span);
        self.coerce(h, ty)
    }

    /// How many of `n` arguments `c`'s constructor gets: all of them for a rest parameter, else
    /// at most one per parameter (JavaScript drops the others), none without a constructor.
    fn species_arity(&self, c: DefId, n: usize) -> usize {
        let Some(ctor) = self.cx.adt(c).and_then(|a| a.ctor) else {
            return 0;
        };
        if crate::body::defaults::has_rest(self.cx, ctor) {
            return n;
        }
        n.min(self.cx.fn_info(ctor).params.len())
    }

    /// Does `new c(args…)` check (a trial: what it adds is undone either way)?
    fn species_fits(&mut self, c: DefId, args: &[ast::Expr], span: Span) -> bool {
        if self.cx.adt(c).is_some_and(|a| !a.generics.names.is_empty()) {
            return false;
        }
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        let args = &args[..self.species_arity(c, args.len())];
        let h = self.new_class(c, vec![], (span, None), args, None, span);
        let ok =
            !self.cx.ty.is_bottom(h.ty) && !self.cx.diags[diags..].iter().any(|d| d.is_error());
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        ok
    }

    /// The warning that the copies `c`'s objects get made with `args` are of its base `base`,
    /// once per list of argument types, where `c` is declared.
    fn species_warning(&mut self, c: DefId, base: DefId, args: &[ast::Expr]) {
        let types: Vec<String> = (args.iter())
            .map(|a| {
                let t = self.species_arg_ty(a);
                self.cx.display(t)
            })
            .collect();
        let (Some(a), Some(b)) = (self.cx.adt(c), self.cx.adt(base)) else {
            return;
        };
        let (cn, bn, at) = (a.name.clone(), b.name.clone(), a.span);
        let msg = format!(
            "the constructor of `{cn}` can't take `({})`, the arguments of `{bn}`'s that built-in methods make copies with: those copies of a `{cn}` are `{bn}`s",
            types.join(", ")
        );
        let note = format!(
            "JavaScript calls `{cn}`'s constructor with them anyway; one whose parameters take them (or with fewer parameters) makes copies of `{cn}`"
        );
        let seen = (self.cx.diags.iter())
            .any(|d| d.message == msg && d.labels.first().map(|l| l.span) == Some(at));
        if !seen {
            self.cx.error(Diagnostic::warning(msg, at).with_note(note));
        }
    }

    /// The type of the plain argument `a`, checked as a trial (it is checked where it is used).
    fn species_arg_ty(&mut self, a: &ast::Expr) -> TyId {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let t = self.expr(a, None, Want::Borrow).ty;
        self.cx.diags.truncate(diags);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        t
    }

    fn species_error(&mut self, msg: &str, span: Span, args: &[ast::Expr]) -> hir::Expr {
        self.cx
            .error(Diagnostic::error(format!("ICE: {msg}"), span));
        self.check_args_loose(args);
        self.error_expr(span)
    }
}

/// Can `e` be evaluated in each arm of the test, once per path: a local's name or a literal?
fn plain(e: &ast::Expr) -> bool {
    matches!(
        e.kind,
        ast::ExprKind::Ident(_) | ast::ExprKind::Lit(_) | ast::ExprKind::This
    )
}
