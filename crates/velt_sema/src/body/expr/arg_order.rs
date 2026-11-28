//! The order in which a call's arguments are checked (`FnCx::check_call`): arguments that fix
//! type parameters first, then those that need them fixed.

use velt_syntax::ast;

use super::args::{as_arrow, deferred, Callable};
use super::arrow_defaults::context_sensitive;
use super::method_value::strip_parens;
use crate::body::FnCx;
use crate::hir::TyId;

impl FnCx<'_, '_> {
    /// The indexes of `args` in the order they are checked.
    pub(super) fn arg_order(&mut self, c: &Callable, args: &[ast::Expr]) -> Vec<usize> {
        // A parameter of a deferred type (`t: IsString<T>`) is known only once the other
        // arguments have fixed its type parameters: its argument goes with the second round.
        // A deferred intersection (`s: Signal<T>`) is a pattern instead: its argument fixes `T`
        // from the object parts' fields, so it goes first, as TypeScript infers from it before
        // checking a literal against `T` (`reset(s, { data: null })`).
        let pattern: Vec<bool> = (0..args.len())
            .map(|i| {
                c.params
                    .get(i)
                    .is_some_and(|p| self.cx.intersection_pattern(p.ty))
            })
            .collect();
        let late: Vec<bool> = (0..args.len())
            .map(|i| {
                c.params
                    .get(i)
                    .is_some_and(|p| self.cx.mentions_op(p.ty) && !pattern[i])
            })
            .collect();
        // A class as a value: the other arguments fix the type parameters first, as TypeScript
        // infers from them (`mk(B, new A())` for `mk<T>(C: new () => T, x: T)` is `T = A`), and
        // the class fixes only those left (`class_arg_slots`); a generic class (`reg(Box, 1)`
        // for `C: new (v: T) => Box<T>`) takes its type arguments from the signature. It goes
        // before the arrows, whose parameters it may type (`run((b) => b.m, B)`).
        let class: Vec<bool> = args.iter().map(|a| self.names_class(a)).collect();
        // A context-sensitive argument comes last: nothing after it can give its parameters a
        // type (`fix_for_context_sensitive`), and everything else is a candidate for them.
        // A bare `undefined` for a `T | undefined` offers no candidate for `T`: it goes after
        // every argument that may offer one (`g(undefined, 1)` is `T = number`), and fixes those
        // still open (`fix_for_context_sensitive`, #2683).
        // So does an object literal that leaves out every optional field naming a type parameter
        // another parameter also names (`f({}, 1)` for `f<S>(o: { input?: S }, x: S)`, #3116).
        let nothing: Vec<bool> = (0..args.len())
            .map(|i| {
                c.params.get(i).is_some_and(|p| {
                    self.bare_nothing_for(&args[i], p.ty)
                        || self.left_out_slots(&args[i], p.ty).iter().any(|&s| {
                            (c.params.iter().enumerate()).any(|(j, q)| {
                                let mut names = vec![];
                                crate::types::collect_params(&self.cx.ty, q.ty, &mut names);
                                j != i && names.contains(&s)
                            })
                        })
                })
            })
            .collect();
        let round = |i: usize| match (deferred(&args[i]) || late[i], as_arrow(&args[i]).is_some()) {
            _ if class[i] => 2,
            _ if context_sensitive(&args[i]) => 5,
            _ if nothing[i] => 4,
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 3,
        };
        let mut order: Vec<usize> = (0..args.len()).collect();
        order.sort_by_key(|&i| (round(i), !pattern[i]));
        order
    }

    /// The type parameters of `pty` that the object literal `arg` offers no candidate for: they
    /// appear only in optional fields it leaves out (`{}` for `{ input?: S }`), as TypeScript
    /// infers nothing from an absent property. Empty for anything else, and for a literal with a
    /// spread or a computed key (what it holds is known only from their types) or a method.
    pub(super) fn left_out_slots(&mut self, arg: &ast::Expr, pty: TyId) -> Vec<u32> {
        let ast::ExprKind::Object(props) = &strip_parens(arg).kind else {
            return vec![];
        };
        let core = self.cx.ty.core(pty).unwrap_or(pty);
        let Some((d, targs)) = self.adt_of(core) else {
            return vec![];
        };
        let mut written: Vec<&str> = vec![];
        for p in props {
            match p {
                ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => {
                    written.push(&k.name)
                }
                ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => return vec![],
            }
        }
        let Some(a) = self.cx.adt(d) else {
            return vec![];
        };
        let fields: Vec<(bool, TyId)> = (a.fields.iter())
            .map(|f| (f.optional && !written.contains(&f.name.as_str()), f.ty))
            .collect();
        let (mut absent, mut present) = (vec![], vec![]);
        for (left_out, t) in fields {
            let t = self.cx.subst(t, &targs);
            let names = if left_out { &mut absent } else { &mut present };
            crate::types::collect_params(&self.cx.ty, t, names);
        }
        absent.retain(|s| !present.contains(s));
        absent.sort_unstable();
        absent.dedup();
        absent
    }
}
