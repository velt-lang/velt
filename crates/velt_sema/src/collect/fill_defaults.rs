//! Default-filling vtable entries: in JavaScript a parameter default belongs to the function
//! that runs, so `b.m()` on a `B` holding a `D` takes the default of `D`'s override. When an
//! override somewhere gives a parameter another default than the method it overrides, the
//! method's vtable slot gets a second slot, `<fill>m`, whose entry in each class is a
//! synthesized method `C.<fill m>` taking those parameters as `T | undefined` and calling `C`'s
//! own `m` directly with its own default where the argument is `undefined`:
//!
//! ```text
//! class D extends B { override m(x: number = 2) }
//!   →  D.<fill m>(this, x: number | undefined) { return D.m(this, x === undefined ? 2 : x) }
//! ```
//!
//! A call that leaves such an argument out (or passes `undefined`, or a value that may be
//! `undefined`) calls the fill slot (`body::expr::virtual_defaults`); a call passing every
//! argument calls the method's own slot as before, so it costs nothing extra. The bodies are
//! made once the defaults can be checked, before the function bodies
//! (`body::expr::virtual_defaults::build_fill_bodies`).

use rustc_hash::FxHashMap;

use velt_syntax::ast;

use super::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{DefId, Nothing, PassMode, TyId};

/// The prefix of a fill slot's vtable key.
pub(crate) const FILL: &str = "<fill>";

/// The vtable key of the fill slot of method key `key`: the one taking the parameters at
/// `Fill::positions` as `T | undefined` (`trailing: None`), or the one taking only the first
/// `k` parameters (`Some(k)`, a call leaving out the rest).
pub(crate) fn fill_key(key: &str, trailing: Option<usize>) -> String {
    match trailing {
        None => format!("{FILL}{key}"),
        Some(k) => format!("{FILL}#{k}#{key}"),
    }
}

/// A fill entry (`Ctx::fills`): the method it calls (key `key`), the parameters a general
/// entry takes as `T | undefined` (positions, not counting `this`), and for an entry of
/// left-out trailing arguments the number it takes (it applies the method's own defaults to the
/// rest).
#[derive(Clone, Debug)]
pub(crate) struct Fill {
    pub method: DefId,
    pub key: String,
    pub positions: Vec<usize>,
    pub trailing: Option<usize>,
}

/// Per (introducing class, method key): the methods overriding it anywhere in the program.
pub(super) type Overrides = FxHashMap<(DefId, String), Vec<DefId>>;

/// The parameters of method `def` whose default may differ in an override: a default the
/// caller places (not one the function applies itself, whose type allows `undefined`), on a
/// parameter whose type holds no nothing. A rest parameter after them is passed on as it is.
fn fillable(cx: &Ctx, def: DefId) -> Vec<usize> {
    let f = cx.fn_info(def);
    let Some(src) = f.source else { return vec![] };
    let sig = crate::body::defaults::fn_sig_ast(src);
    (sig.params.iter().enumerate())
        .filter(|(k, p)| {
            let Some(ps) = f.params.get(*k) else {
                return false;
            };
            p.default.is_some()
                && !p.optional
                && !p.rest
                && cx.ty.nothing_of(ps.ty).is_none()
                && cx.dynamic(ps.ty).is_none()
                && !cx.ty.has_error(ps.ty)
        })
        .map(|(k, _)| k)
        .collect()
}

/// The default of parameter `k` of method `def` as written.
fn written_default<'m>(cx: &Ctx<'m>, def: DefId, k: usize) -> Option<&'m ast::Expr> {
    let src = cx.fn_info(def).source?;
    let p = crate::body::defaults::fn_sig_ast(src).params.get(k)?;
    p.default.as_ref().filter(|_| !p.optional)
}

/// The parameters of method `def` (introduced by its class, key `key`) that an override gives
/// another default; empty when none does.
pub(super) fn fill_positions(
    cx: &Ctx,
    overrides: &Overrides,
    d: DefId,
    key: &str,
    def: DefId,
) -> Vec<usize> {
    let Some(subs) = overrides.get(&(d, key.to_string())) else {
        return vec![];
    };
    fillable(cx, def)
        .into_iter()
        .filter(|&k| {
            let base = written_default(cx, def, k);
            subs.iter()
                .any(|&m| match (base, written_default(cx, m, k)) {
                    (Some(a), Some(b)) => !same_literal(a, b),
                    _ => false,
                })
        })
        .collect()
}

/// The numbers of leading arguments a call of method `def` may pass and leave the rest to
/// defaults, up to the last parameter at `positions` (each gets a fill slot of its own): from
/// the first parameter after which every one has a default or is optional.
pub(super) fn trailing_counts(cx: &Ctx, def: DefId, positions: &[usize]) -> Vec<usize> {
    let Some(src) = cx.fn_info(def).source else {
        return vec![];
    };
    let ps = &crate::body::defaults::fn_sig_ast(src).params;
    let Some(&last) = positions.iter().max() else {
        return vec![];
    };
    let mut first = ps.len();
    while first > 0 && (ps[first - 1].default.is_some() || ps[first - 1].optional) {
        first -= 1;
    }
    (first..=last).collect()
}

/// The fill entry of class `d` for its method `m` (key `key`, vtable entry of the slot): the
/// general one, taking the parameters at `positions` as `T | undefined`, or (`trailing:
/// Some(k)`) the one taking the first `k` parameters.
pub(super) fn fill_method(
    cx: &mut Ctx,
    d: DefId,
    (m, key): (DefId, &str),
    positions: &[usize],
    trailing: Option<usize>,
) -> DefId {
    let host = Host::adt(cx, d);
    let n = host.generics.len();
    let self_ty = super::self_type(cx, d, n);
    let self_args: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let f = cx.fn_info(m);
    let mut_this = f
        .this
        .as_ref()
        .is_some_and(|t| t.mode == PassMode::BorrowMut);
    let (ret, params) = (f.ret, f.params.clone());
    let short = f.name.rsplit('.').next().unwrap_or("").to_string();
    let taken = trailing.unwrap_or(params.len());
    let params: Vec<ParamSig> = (params.into_iter().enumerate())
        .take(taken)
        .map(|(k, mut p)| {
            if trailing.is_none() && positions.contains(&k) {
                p.ty = cx.ty.option_of(p.ty, Nothing::Undefined);
                if cx.is_copy(p.ty) {
                    p.mode = PassMode::Copy;
                }
            }
            p.default = None;
            p
        })
        .collect();
    let qual = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
    let def = synth_method(
        cx,
        &host,
        self_ty,
        format!("{qual}.<fill {short}>"),
        params,
        ret,
        mut_this,
        Target::Default(m, self_args),
    );
    cx.fills.insert(
        def,
        Fill {
            method: m,
            key: key.to_string(),
            positions: positions.to_vec(),
            trailing,
        },
    );
    def
}

/// Are defaults `a` and `b` the same literal (`= 1` in both)?
fn same_literal(a: &ast::Expr, b: &ast::Expr) -> bool {
    match (&a.kind, &b.kind) {
        (ast::ExprKind::Paren(x), _) => same_literal(x, b),
        (_, ast::ExprKind::Paren(y)) => same_literal(a, y),
        (ast::ExprKind::Lit(x), ast::ExprKind::Lit(y)) => x == y,
        (ast::ExprKind::Unary { op: x, expr: ex }, ast::ExprKind::Unary { op: y, expr: ey }) => {
            x == y && same_literal(ex, ey)
        }
        _ => false,
    }
}
