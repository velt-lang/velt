//! The constant folder of decorator arguments: an expression to a [`ConstTree`] (literals, enum
//! members, arrays and object literals of constants, references to `const` declarations), and
//! whether a tree fits a parameter's type.

use std::cell::{Cell, RefCell};

use velt_common::Span;
use velt_syntax::ast;

use super::scope;
use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{ConstTree, DefId, FloatTy, IntTy, LitValue, TyId, TyKind};

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

/// Why an expression has no value: the part of it that isn't constant, and when that is a name
/// imported with `import type`, the module it is imported into and the name; or an object literal
/// of the expression itself (not of a `const` it names, which its own check reports) giving a
/// property twice, and its name.
pub(super) struct NotConstant {
    pub span: Span,
    pub type_only: Option<(usize, String)>,
    pub duplicate: Option<String>,
}

/// The value of `e` in module `m`.
pub(super) fn value_of(cx: &Ctx, m: usize, e: &ast::Expr) -> Result<ConstTree, NotConstant> {
    let f = Fold {
        cx,
        nodes: Cell::new(0),
        type_only: RefCell::new(None),
        duplicate: RefCell::new(None),
    };
    eval(&f, m, e, 0).map_err(
        |span| match (f.type_only.into_inner(), f.duplicate.into_inner()) {
            (Some((module, name, at)), _) => NotConstant {
                span: at,
                type_only: Some((module, name)),
                duplicate: None,
            },
            (None, Some((at, name))) => NotConstant {
                span: at,
                type_only: None,
                duplicate: Some(name),
            },
            (None, None) => NotConstant {
                span,
                type_only: None,
                duplicate: None,
            },
        },
    )
}

struct Fold<'c, 'm> {
    cx: &'c Ctx<'m>,
    nodes: Cell<u32>,
    /// The first name imported with `import type` that the expression reaches: its module, the
    /// name, and where.
    type_only: RefCell<Option<(usize, String, Span)>>,
    /// A property an object literal of the argument itself gives twice: where, and its name.
    duplicate: RefCell<Option<(Span, String)>>,
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
        // Sixteen numbers or more, which the parser packs.
        ast::ExprKind::NumArray(_) => each(&e.unpacked())?,
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
            if let Some((name, span)) = repeated_key(props) {
                if depth == 0 {
                    f.duplicate.borrow_mut().get_or_insert((span, name));
                }
                return Err(span);
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

/// The first property object literal `props` gives a second time, and where.
fn repeated_key(props: &[ast::ObjectProp]) -> Option<(String, Span)> {
    let mut seen = rustc_hash::FxHashSet::default();
    props.iter().find_map(|p| {
        let k = match p {
            ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => k,
            _ => return None,
        };
        (!seen.insert(k.name.as_str())).then(|| (k.name.clone(), k.span))
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
    let head = path.first().ok_or(last.span)?;
    // A local hides any constant of its name, and no local is a constant here: its value belongs
    // to one run of the function.
    if scope::shadow_of(cx, m, head) == Some(scope::Shadow::Local) {
        return Err(last.span);
    }
    if let Some(name) = scope::type_only_name(cx, m, path) {
        f.type_only.borrow_mut().get_or_insert((m, name, head.span));
        return Err(last.span);
    }
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

/// The `const` `path` names in module `m` (`POLICY`, `ns.POLICY`, a `static readonly` field), for
/// [`super::suffixes`]; `None` for anything else, a local of its name in scope included.
pub(super) fn constant_named(cx: &Ctx, m: usize, path: &[ast::Ident]) -> Option<DefId> {
    let last = path.last()?;
    if scope::shadow_of(cx, m, path.first()?) == Some(scope::Shadow::Local) {
        return None;
    }
    let d = match cx.lookup_path_at(m, path, last.span) {
        Some(Item::Def(d)) => d,
        _ => {
            let (member, owner) = path.split_last()?;
            let Some(Item::Def(owner)) = cx.lookup_path_at(m, owner, last.span) else {
                return None;
            };
            let DefInfo::Adt(a) = &cx.info[owner.0 as usize] else {
                return None;
            };
            *a.statics.get(&member.name)?
        }
    };
    matches!(&cx.info[d.0 as usize], DefInfo::Global(g) if !g.mutable).then_some(d)
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

/// The number or the string enum member `v` stands for (`None`: not a member, or one with a
/// payload).
fn enum_value(cx: &Ctx, v: &ConstTree) -> Option<ConstTree> {
    let ConstTree::EnumMember(e, i) = v else {
        return None;
    };
    let DefInfo::Enum(info) = &cx.info[e.0 as usize] else {
        return None;
    };
    let member = info.variants.get(*i as usize).filter(|_| !info.is_union)?;
    if !member.payload.is_empty() {
        return None;
    }
    Some(match &member.str_value {
        Some(s) => ConstTree::Str(s.clone()),
        None => ConstTree::Int(member.discriminant.into()),
    })
}

/// May a float type `t` hold `v`? Within its range, rounded to its nearest value: a number past
/// its largest finite one (`1e300` for an `f32`) would be an infinity, so it doesn't fit; an
/// infinity or a NaN written as such does.
fn float_fits(t: FloatTy, v: f64) -> bool {
    let max = match t {
        FloatTy::F64 => return true,
        FloatTy::F32 => f32::MAX as f64,
        FloatTy::F16 => 65504.0,
        FloatTy::BF16 => 3.389_531_389_251_535_5e38,
    };
    !v.is_finite() || v.abs() <= max
}

/// `v` as a parameter of type `ty` records it: an enum member as its number or string where the
/// parameter is a number or a string, so a reader finds what the type says.
pub(super) fn as_recorded(cx: &Ctx, v: ConstTree, ty: TyId) -> ConstTree {
    use ConstTree as C;
    let kind = cx.ty.kind(ty);
    match (kind, v) {
        (
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Literal(_),
            v @ C::EnumMember(..),
        ) => enum_value(cx, &v).unwrap_or(v),
        (TyKind::Option(inner, _), v) => as_recorded(cx, v, *inner),
        (TyKind::Array(elem), C::Array(items)) => C::Array(
            items
                .into_iter()
                .map(|x| as_recorded(cx, x, *elem))
                .collect(),
        ),
        (TyKind::Tuple(elems), C::Array(items)) => C::Array(
            items
                .into_iter()
                .zip(elems.iter())
                .map(|(x, t)| as_recorded(cx, x, *t))
                .collect(),
        ),
        (TyKind::Adt(d, _), C::Object(props)) => match &cx.info[d.0 as usize] {
            DefInfo::Adt(a) => C::Object(
                props
                    .into_iter()
                    .map(|(name, x)| {
                        let field = a.fields.iter().find(|f| f.name == name);
                        let x = match field {
                            Some(f) => as_recorded(cx, x, f.ty),
                            None => x,
                        };
                        (name, x)
                    })
                    .collect(),
            ),
            _ => C::Object(props),
        },
        (_, v) => v,
    }
}

/// May a parameter of type `ty` hold `v`?
pub(super) fn fits(cx: &Ctx, v: &ConstTree, ty: TyId) -> bool {
    use ConstTree as C;
    if cx.dynamic(ty).is_some() {
        // `any` and `unknown` hold every value.
        return true;
    }
    // A member of a numeric or string enum is its number or its string (TypeScript's `E.A` where
    // a `number` or a `string` is expected).
    let plain = matches!(
        cx.ty.kind(ty),
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Literal(_)
    );
    if plain && matches!(v, C::EnumMember(..)) {
        return enum_value(cx, v).is_some_and(|plain| fits(cx, &plain, ty));
    }
    match cx.ty.kind(ty) {
        TyKind::Error => true,
        TyKind::Int(t) => matches!(v, C::Int(i) if int_fits(*t, *i)),
        TyKind::Float(t) => match v {
            C::Int(i) => float_fits(*t, *i as f64),
            C::Float(bits) => float_fits(*t, f64::from_bits(*bits)),
            _ => false,
        },
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
