//! The order in which a call's arguments are checked (`FnCx::check_call`): arguments that fix
//! type parameters first, then those that need them fixed.

use velt_syntax::ast;

use super::args::{as_arrow, deferred, Callable};
use super::arrow_defaults::context_sensitive;
use super::method_value::strip_parens;
use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

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
                        || self.left_out_slots(&args[i], p.ty).all().any(|s| {
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

    /// What the object literal `arg` offers the type parameters of `pty` (`LeftOut`): nothing for
    /// those that appear only in optional fields it leaves out (`{}` for `{ input?: S }`, also
    /// inside a nested literal: `{ opts: {} }` for `{ opts: { input?: S } }`), as TypeScript
    /// infers nothing from an absent property, and `undefined` (or `null`) for one that is the
    /// whole type of an optional field written as `undefined` (`{ input: undefined }`) or `null`.
    /// Empty for anything
    /// else, and for a literal with a spread or a computed key (what it holds is known only from
    /// their types) or a method.
    pub(super) fn left_out_slots(&mut self, arg: &ast::Expr, pty: TyId) -> LeftOut {
        let mut parts = Parts::default();
        if !self.literal_parts(arg, pty, &mut parts) {
            return LeftOut::default();
        }
        let Parts {
            mut none,
            mut undefined,
            mut null,
            present,
        } = parts;
        // A slot written both ways (`{ a: null, b: undefined }`) is left to the rest.
        let both: Vec<u32> = undefined
            .iter()
            .filter(|s| null.contains(s))
            .copied()
            .collect();
        undefined.retain(|s| !present.contains(s) && !both.contains(s));
        null.retain(|s| !present.contains(s) && !both.contains(s));
        none.retain(|s| !present.contains(s) && !undefined.contains(s) && !null.contains(s));
        for v in [&mut none, &mut undefined, &mut null] {
            v.sort_unstable();
            v.dedup();
        }
        LeftOut {
            none,
            undefined,
            null,
        }
    }

    /// The type parameters of `pty` the fields of object literal `arg` name, by what the literal
    /// gives each field (`left_out_slots`); `false` when `arg` is no such literal.
    fn literal_parts(&mut self, arg: &ast::Expr, pty: TyId, parts: &mut Parts) -> bool {
        let ast::ExprKind::Object(props) = &strip_parens(arg).kind else {
            return false;
        };
        let core = self.cx.ty.core(pty).unwrap_or(pty);
        let Some((d, targs)) = self.adt_of(core) else {
            return false;
        };
        let mut written: Vec<(&str, Option<&ast::Expr>)> = vec![];
        for p in props {
            match p {
                ast::ObjectProp::KeyValue(k, v) => written.push((&k.name, Some(v))),
                ast::ObjectProp::Shorthand(k) => written.push((&k.name, None)),
                ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => return false,
            }
        }
        let Some(a) = self.cx.adt(d) else {
            return false;
        };
        let fields: Vec<(String, bool, TyId)> = (a.fields.iter())
            .map(|f| (f.name.clone(), f.optional, f.ty))
            .collect();
        for (name, optional, t) in fields {
            let t = self.cx.subst(t, &targs);
            let value = written.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
            match value {
                None if optional => crate::types::collect_params(&self.cx.ty, t, &mut parts.none),
                Some(Some(v)) if optional && nothing_lit(v).is_some() => {
                    let core = self.cx.ty.core(t).unwrap_or(t);
                    let param = match *self.cx.ty.kind(core) {
                        TyKind::Param(s) => Some(s),
                        _ => None,
                    };
                    match (param, nothing_lit(v)) {
                        (Some(s), Some(ast::Lit::Undefined)) => parts.undefined.push(s),
                        (Some(s), Some(_)) => parts.null.push(s),
                        _ => crate::types::collect_params(&self.cx.ty, t, &mut parts.none),
                    }
                }
                Some(Some(v)) if self.literal_parts(v, t, parts) => {}
                _ => crate::types::collect_params(&self.cx.ty, t, &mut parts.present),
            }
        }
        true
    }
}

/// The type parameters an object literal argument offers nothing (`none`), only `undefined`
/// (`undefined`) or only `null` (`null`) for (`FnCx::left_out_slots`).
#[derive(Default)]
pub(super) struct LeftOut {
    pub none: Vec<u32>,
    pub undefined: Vec<u32>,
    pub null: Vec<u32>,
}

impl LeftOut {
    /// Every slot the literal leaves to the other arguments.
    pub(super) fn all(&self) -> impl Iterator<Item = u32> + '_ {
        (self.none.iter().chain(&self.undefined).chain(&self.null)).copied()
    }
}

/// What `literal_parts` collects, by field.
#[derive(Default)]
struct Parts {
    none: Vec<u32>,
    undefined: Vec<u32>,
    null: Vec<u32>,
    present: Vec<u32>,
}

/// `e` as the literal `undefined` or `null`.
fn nothing_lit(e: &ast::Expr) -> Option<&ast::Lit> {
    match &strip_parens(e).kind {
        ast::ExprKind::Lit(l @ (ast::Lit::Undefined | ast::Lit::Null)) => Some(l),
        _ => None,
    }
}
