//! The order in which a call's arguments are checked (`FnCx::check_call`): arguments that fix
//! type parameters first, then those that need them fixed.

use velt_syntax::ast;

use super::args::{as_arrow, deferred, Callable};
use super::arrow_defaults::context_sensitive;
use crate::body::FnCx;

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
        let nothing: Vec<bool> = (0..args.len())
            .map(|i| {
                c.params
                    .get(i)
                    .is_some_and(|p| self.bare_nothing_for(&args[i], p.ty))
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
}
