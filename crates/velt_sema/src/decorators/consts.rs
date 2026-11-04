//! The constant folder of decorator arguments: an expression to a [`ConstTree`] (literals, enum
//! members, arrays and object literals of constants, references to `const` declarations), and
//! whether a tree fits a parameter's type.

use std::cell::Cell;

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{ConstTree, DefId, IntTy, LitValue, TyId, TyKind};

/// How many `const`s may refer to each other in one argument: far more than code writes, so only
/// a cycle reaches it.
const MAX_DEPTH: u32 = 64;
/// How many expressions one argument may fold to, through `const`s that each hold the next one
/// twice (which would otherwise double per `const`).
const MAX_NODES: u32 = 1 << 16;

/// The dotted path `e` is (`a`, `a.b.c`), or `None`.
pub(super) fn path_of(e: &ast::Expr) -> Option<Vec<ast::Ident>> {
    match &e.kind {
        ast::ExprKind::Ident(id) => Some(vec![id.clone()]),
        ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } => {
            let mut path = path_of(object)?;
            path.push(prop.clone());
            Some(path)
        }
        _ => None,
    }
}

/// The value of `e` in module `m`; on failure, the part of it that isn't constant.
pub(super) fn value_of(cx: &Ctx, m: usize, e: &ast::Expr) -> Result<ConstTree, Span> {
    eval(
        &Fold {
            cx,
            nodes: Cell::new(0),
        },
        m,
        e,
        0,
    )
}

struct Fold<'c, 'm> {
    cx: &'c Ctx<'m>,
    nodes: Cell<u32>,
}

fn eval(f: &Fold, m: usize, e: &ast::Expr, depth: u32) -> Result<ConstTree, Span> {
    f.nodes.set(f.nodes.get() + 1);
    if depth > MAX_DEPTH || f.nodes.get() > MAX_NODES {
        return Err(e.span);
    }
    let each = |e| eval(f, m, e, depth);
    Ok(match &e.kind {
        ast::ExprKind::Lit(lit) => match lit {
            ast::Lit::Int { value, .. } => {
                ConstTree::Int(i128::try_from(*value).map_err(|_| e.span)?)
            }
            ast::Lit::Float { value, .. } => ConstTree::Float(value.to_bits()),
            ast::Lit::Str(s) => ConstTree::Str(s.clone()),
            ast::Lit::Bool(b) => ConstTree::Bool(*b),
            ast::Lit::Null => ConstTree::Null,
            ast::Lit::Undefined => ConstTree::Undefined,
        },
        ast::ExprKind::Template { quasis, exprs } if exprs.is_empty() => {
            ConstTree::Str(quasis.concat())
        }
        ast::ExprKind::Paren(inner) => each(inner)?,
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => match each(expr)? {
            ConstTree::Int(v) => ConstTree::Int(-v),
            ConstTree::Float(bits) => ConstTree::Float((-f64::from_bits(bits)).to_bits()),
            _ => return Err(e.span),
        },
        ast::ExprKind::Cast { expr, ty } if is_as_const(ty) => each(expr)?,
        ast::ExprKind::Array(items) => {
            ConstTree::Array(items.iter().map(each).collect::<Result<_, _>>()?)
        }
        ast::ExprKind::Object(props) => {
            let mut out = Vec::with_capacity(props.len());
            for p in props {
                out.push(match p {
                    ast::ObjectProp::KeyValue(k, v) => (k.name.clone(), each(v)?),
                    ast::ObjectProp::Shorthand(k) => {
                        (k.name.clone(), named(f, m, std::slice::from_ref(k), depth)?)
                    }
                    ast::ObjectProp::Spread(v) => return Err(v.span),
                    ast::ObjectProp::Method(f) => return Err(f.sig.name.span),
                });
            }
            ConstTree::Object(out)
        }
        ast::ExprKind::Ident(_) | ast::ExprKind::Member { .. } => {
            let path = path_of(e).ok_or(e.span)?;
            named(f, m, &path, depth).map_err(|_| e.span)?
        }
        _ => return Err(e.span),
    })
}

fn is_as_const(ty: &ast::TypeExpr) -> bool {
    matches!(&ty.kind, ast::TypeExprKind::Named { path, .. } if matches!(path.as_slice(), [c] if c.name == "const"))
}

/// The value `path` names: a `const` (`POLICY`, `ns.POLICY`, a `static readonly` field) or an enum
/// member (`Color.Red`, `ns.Color.Red`).
fn named(f: &Fold, m: usize, path: &[ast::Ident], depth: u32) -> Result<ConstTree, Span> {
    let cx = f.cx;
    let last = path.last().ok_or(Span::DUMMY)?;
    if let Some(Item::Def(d)) = cx.lookup_path_at(m, path, last.span) {
        return global_value(f, d, depth).ok_or(last.span);
    }
    let (member, owner) = path.split_last().ok_or(last.span)?;
    let Some(Item::Def(owner)) = cx.lookup_path_at(m, owner, last.span) else {
        return Err(last.span);
    };
    match &cx.info[owner.0 as usize] {
        DefInfo::Enum(e) => {
            let i = e.variants.iter().position(|v| v.name == member.name);
            i.map(|i| ConstTree::EnumMember(owner, i as u32))
                .ok_or(last.span)
        }
        DefInfo::Adt(a) => {
            let g = a.statics.get(&member.name).ok_or(last.span)?;
            global_value(f, *g, depth).ok_or(last.span)
        }
        _ => Err(last.span),
    }
}

/// The value of constant `d`; `None` for anything else.
fn global_value(f: &Fold, d: DefId, depth: u32) -> Option<ConstTree> {
    let DefInfo::Global(g) = &f.cx.info[d.0 as usize] else {
        return None;
    };
    if g.mutable {
        return None;
    }
    eval(f, g.module, g.src.init?, depth + 1).ok()
}

fn int_fits(t: IntTy, v: i128) -> bool {
    let bits = t.bits().min(64);
    match t.is_signed() {
        true => (-(1i128 << (bits - 1))..1i128 << (bits - 1)).contains(&v),
        false => (0..1i128 << bits).contains(&v),
    }
}

/// May a parameter of type `ty` hold `v`?
pub(super) fn fits(cx: &Ctx, v: &ConstTree, ty: TyId) -> bool {
    use ConstTree as C;
    match cx.ty.kind(ty) {
        TyKind::Error => true,
        TyKind::Int(t) => matches!(v, C::Int(i) if int_fits(*t, *i)),
        TyKind::Float(_) => matches!(v, C::Int(_) | C::Float(_)),
        TyKind::Bool => matches!(v, C::Bool(_)),
        TyKind::Str => matches!(v, C::Str(_)),
        TyKind::Literal(lit) => match (lit, v) {
            (LitValue::Str(a), C::Str(b)) => a == b,
            (LitValue::Bool(a), C::Bool(b)) => a == b,
            (LitValue::Int(_, a), C::Int(b)) => a == b,
            (LitValue::Float(_, a), C::Float(b)) => a == b,
            (LitValue::Float(_, a), C::Int(b)) => *a == (*b as f64).to_bits(),
            _ => false,
        },
        TyKind::Option(inner, _) => matches!(v, C::Null | C::Undefined) || fits(cx, v, *inner),
        TyKind::Array(elem) => {
            matches!(v, C::Array(items) if items.iter().all(|x| fits(cx, x, *elem)))
        }
        TyKind::Tuple(elems) => matches!(v, C::Array(items)
            if items.len() == elems.len()
                && items.iter().zip(elems).all(|(x, t)| fits(cx, x, *t))),
        TyKind::Adt(d, _) => match &cx.info[d.0 as usize] {
            DefInfo::Enum(e) if e.is_union => e
                .variants
                .iter()
                .any(|m| m.payload.iter().any(|t| fits(cx, v, *t))),
            DefInfo::Enum(_) => matches!(v, C::EnumMember(of, _) if of == d),
            DefInfo::Adt(a) if a.kind != crate::hir::AdtKind::Class => {
                let C::Object(props) = v else { return false };
                let known = props.iter().all(|(name, value)| {
                    a.fields
                        .iter()
                        .find(|f| f.name == *name)
                        .is_some_and(|f| fits(cx, value, f.ty))
                });
                let complete = (a.fields.iter()).all(|f| {
                    f.has_default || f.optional || props.iter().any(|(n, _)| *n == f.name)
                });
                known && complete
            }
            _ => false,
        },
        _ => false,
    }
}
