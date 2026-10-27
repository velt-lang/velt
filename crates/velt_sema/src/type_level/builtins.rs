//! The built-in type-level operators: `NonNullable`, `Awaited`, `Exclude` / `Extract` and the
//! string intrinsics (`Uppercase`, `Lowercase`, `Capitalize`, `Uncapitalize`).

use velt_syntax::ast;

use super::{unknown_name, Subject};
use crate::ctx::Ctx;
use crate::hir::{LitValue, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// The built-in type-level operators of this module, `None` for other names.
    pub(crate) fn resolve_type_level_builtin(
        &mut self,
        t: &ast::TypeExpr,
        name: &str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Option<TyId> {
        let arity = match name {
            "NonNullable" | "Awaited" | "Uppercase" | "Lowercase" | "Capitalize"
            | "Uncapitalize" => 1,
            "Exclude" | "Extract" => 2,
            _ => return None,
        };
        if args.len() != arity {
            self.err(
                format!(
                    "type `{name}` takes {arity} type argument(s) but {} were supplied",
                    args.len()
                ),
                t.span,
            );
            return Some(self.ty.error);
        }
        // `NonNullable<M>` of an `infer M` that found nothing (`unknown`) is `{}`.
        if name == "NonNullable" && unknown_name(&args[0], env) {
            return Some(self.anon_type_with(&[], env.module));
        }
        let a = self.resolve_type(&args[0], env);
        let Some(a) = self.known_type(a, env, &format!("`{name}`"), args[0].span) else {
            return Some(self.ty.error);
        };
        Some(match name {
            "NonNullable" => match *self.ty.kind(a) {
                TyKind::Option(p, _) => p,
                _ => a,
            },
            // What `await` gives: the value of a promise (of a promise ...), for each member.
            "Awaited" => {
                let mut kept = vec![];
                let mut nothing: Option<Nothing> = None;
                self.awaited_members(a, &mut kept, &mut nothing, 0);
                self.rejoin_booleans(&mut kept);
                match (kept.as_slice(), nothing) {
                    ([one], None) => *one,
                    _ => self.union_with(&kept, nothing, t.span),
                }
            }
            "Exclude" | "Extract" => {
                let keep_matching = name == "Extract";
                let mut kept = vec![];
                let mut nothing: Option<Nothing> = None;
                for s in self.subjects_of(a) {
                    let Ok(found) = self.match_pattern(s, &args[1], env) else {
                        return Some(self.ty.error);
                    };
                    if found.is_some() != keep_matching {
                        continue;
                    }
                    match s {
                        Subject::Ty(m) => kept.push(m),
                        Subject::Nothing(n) => nothing = Some(nothing.map_or(n, |m| m.join(n))),
                    }
                }
                self.rejoin_booleans(&mut kept);
                match (kept.as_slice(), nothing) {
                    ([one], None) => *one,
                    _ => self.union_with(&kept, nothing, t.span),
                }
            }
            _ => {
                if a == self.ty.str_ {
                    return Some(a);
                }
                let Some(strs) = self.literal_strings(a) else {
                    let shown = self.display(a);
                    self.err(
                        format!("`{name}` needs a string literal type; found `{shown}`"),
                        args[0].span,
                    );
                    return Some(self.ty.error);
                };
                let lits: Vec<TyId> = strs
                    .iter()
                    .map(|s| {
                        let s = string_intrinsic(name, s);
                        self.lit_type(LitValue::Str(s))
                    })
                    .collect();
                match lits.as_slice() {
                    [one] => *one,
                    _ => self.union_of(&lits, false, t.span),
                }
            }
        })
    }

    /// The members of `Awaited<t>` into `kept` and `nothing`: each member of `t`, a promise's
    /// value in its place, unwrapped again member by member (`Promise<Promise<A> | B>` is
    /// `A | B`).
    fn awaited_members(
        &mut self,
        t: TyId,
        kept: &mut Vec<TyId>,
        nothing: &mut Option<Nothing>,
        depth: u32,
    ) {
        for s in self.subjects_of(t) {
            match s {
                Subject::Ty(m) => match *self.ty.kind(m) {
                    TyKind::Promise(v, _) if depth < 64 => {
                        self.awaited_members(v, kept, nothing, depth + 1)
                    }
                    TyKind::Option(p, n) => {
                        *nothing = Some(nothing.map_or(n, |x| x.join(n)));
                        self.awaited_members(p, kept, nothing, depth + 1);
                    }
                    _ if !kept.contains(&m) => kept.push(m),
                    _ => {}
                },
                Subject::Nothing(n) => *nothing = Some(nothing.map_or(n, |x| x.join(n))),
            }
        }
    }
}

/// `Uppercase`, `Lowercase`, `Capitalize`, `Uncapitalize` of `s`.
fn string_intrinsic(name: &str, s: &str) -> String {
    let mut cs = s.chars();
    match name {
        "Uppercase" => s.to_uppercase(),
        "Lowercase" => s.to_lowercase(),
        "Capitalize" => match cs.next() {
            Some(c) => c.to_uppercase().chain(cs).collect(),
            None => String::new(),
        },
        _ => match cs.next() {
            Some(c) => c.to_lowercase().chain(cs).collect(),
            None => String::new(),
        },
    }
}
