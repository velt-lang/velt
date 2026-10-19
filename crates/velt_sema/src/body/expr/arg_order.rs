//! The order in which a call's arguments are checked (`FnCx::check_call`): arguments that fix
//! type parameters first, then those that need them fixed.

use velt_syntax::ast;

use super::args::{as_arrow, deferred, Callable};
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
        let round = |i: usize| match (deferred(&args[i]) || late[i], as_arrow(&args[i]).is_some()) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let mut order: Vec<usize> = (0..args.len()).collect();
        order.sort_by_key(|&i| (round(i), !pattern[i]));
        order
    }
}
