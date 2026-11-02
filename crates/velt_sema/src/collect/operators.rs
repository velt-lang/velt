//! Declarations of operator methods (operators.md, #2050; `velt_syntax::operators`): each has
//! the shape its operator calls, and user code gives none to a type whose operators are built in.
//!
//! - `[Symbol.add]` & co.: one parameter (the right operand), or `static` with two, the first
//!   of a type other than the declaring one (the left operand of `2 * v`).
//! - `[Symbol.neg]`, `[Symbol.bitNot]`: no parameter.
//! - `[Symbol.addAssign]` & co.: one parameter, `void` (it changes `this`).
//! - `[Symbol.index]`: one or more parameters. `[Symbol.setIndex]`: those and the value, `void`,
//!   the value of the type `[Symbol.index]` returns when both exist.
//!
//! Every one is an instance method (but the binary static form), synchronous, with plain
//! parameters. A `+` method taking a `string` can never run (`+` with a string operand is
//! concatenation), so it is an error too.

use velt_common::Diagnostic;
use velt_syntax::ast::BinaryOp;

use crate::ctx::Ctx;
use crate::defs::{FnSource, MethodRef, RetSource};
use crate::hir::{DefId, TyId, TyKind};
use velt_syntax::operators::{op_method, OpMethod, INDEX, SET_INDEX};

/// The methods of one declaring type: a class or struct, or an `extend` block.
struct Owner {
    /// The declaring type (the `extend` block's target).
    ty: TyId,
    methods: Vec<(String, MethodRef)>,
    /// An `extend` block, which user code may not write for a type with built-in operators.
    extend: bool,
}

pub(super) fn check(cx: &mut Ctx) {
    let owners = owners(cx);
    for o in &owners {
        check_owner(cx, o);
    }
}

/// Every class, struct and `extend` block that declares operator methods.
fn owners(cx: &mut Ctx) -> Vec<Owner> {
    let mut owners = vec![];
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        let Some(a) = cx.adt(d) else {
            continue;
        };
        let (ms, n) = (operator_methods(a.methods.iter()), a.generics.len());
        if !ms.is_empty() {
            owners.push(Owner {
                ty: super::self_type(cx, d, n),
                methods: ms,
                extend: false,
            });
        }
    }
    for x in &cx.extensions {
        let ms = operator_methods(x.methods.iter());
        if !ms.is_empty() {
            owners.push(Owner {
                ty: x.target,
                methods: ms,
                extend: true,
            });
        }
    }
    owners
}

/// The operator methods of a method table (its overload signatures by their operator's name).
fn operator_methods<'a>(
    ms: impl Iterator<Item = (&'a String, &'a MethodRef)>,
) -> Vec<(String, MethodRef)> {
    let mut out: Vec<(String, MethodRef)> = ms
        .filter_map(|(k, m)| {
            // Most methods have plain names: one test of the first byte skips them.
            if !(k.starts_with('[') || k.starts_with("static [")) {
                return None;
            }
            let base = crate::overloads::sig_of(k).unwrap_or(k);
            let base = base.strip_prefix("static ").unwrap_or(base);
            op_method(base).map(|_| (base.to_string(), *m))
        })
        .collect();
    out.sort_by_key(|(k, m)| (k.clone(), m.def.0));
    out.dedup_by_key(|(_, m)| m.def);
    out
}

fn check_owner(cx: &mut Ctx, o: &Owner) {
    for (name, m) in &o.methods {
        let Some(kind) = op_method(name) else {
            continue;
        };
        let f = cx.fn_info(m.def);
        let (span, std) = (f.name_span, cx.scopes[f.module].is_std);
        // std gives the number types operator methods for generic code (`velt/ops`).
        if o.extend && !std && !operator_target(cx, o.ty) {
            let shown = cx.display(o.ty);
            cx.error(
                Diagnostic::error(
                    format!("an operator method cannot be added to `{shown}`, whose operators are built in"),
                    span,
                )
                .with_note("operator methods belong to classes, structs and type parameters: an operator never changes what it means on numbers, strings, arrays or other built-in types"),
            );
            continue;
        }
        if let Some(why) = shape_error(cx, o.ty, name, kind, m) {
            cx.error(Diagnostic::error(why, span));
        }
    }
    index_pair(cx, o);
}

/// Can type `t` (an `extend` block's target) have operator methods?
fn operator_target(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => cx
            .adt(*d)
            .is_some_and(|a| a.kind != crate::hir::AdtKind::Anon),
        TyKind::Param(_) | TyKind::Simd(_) => true,
        _ => false,
    }
}

/// What is wrong with operator method `m` (named `name`, an operator of kind `kind`) declared
/// by `owner`, if anything.
fn shape_error(cx: &Ctx, owner: TyId, name: &str, kind: OpMethod, m: &MethodRef) -> Option<String> {
    let f = cx.fn_info(m.def);
    if f.is_async || f.is_generator || f.is_getter {
        return Some(format!(
            "an operator method has plain parameters and runs synchronously: `{name}` cannot be async, a generator or an accessor"
        ));
    }
    if let Some(FnSource::Decl(d)) = f.source {
        if d.sig
            .params
            .iter()
            .any(|p| p.optional || p.rest || p.default.is_some())
        {
            return Some(format!(
                "an operator method has plain parameters: `{name}` takes no optional, default or rest parameter"
            ));
        }
    }
    let n = f.params.len();
    let unit = cx.ty.unit;
    // A body without a written result returns `void` unless it returns a value.
    let returns_void = f.ret == unit || f.ret_source == RetSource::Body && cx.ty.is_bottom(f.ret);
    match kind {
        OpMethod::Binary(op) if m.is_static => {
            if n != 2 {
                return Some(format!(
                    "a static `{name}` takes two parameters, the left and the right operand"
                ));
            }
            let (left, right) = (f.params[0].ty, f.params[1].ty);
            if left == owner {
                let shown = cx.display(owner);
                let other = cx.display(right);
                return Some(format!(
                    "`{shown} {} {other}` is an instance method: `{name}(other: {other})` (the static form is for a left operand of another type)",
                    op_symbol(op)
                ));
            }
            if op == BinaryOp::Add && left == cx.ty.str_ {
                return Some(unreachable_concat());
            }
            None
        }
        OpMethod::Binary(op) => {
            if n != 1 {
                return Some(format!("`{name}` takes one parameter, the right operand"));
            }
            // std's `extend string` gives `string` its `+` for generic code (`velt/ops`).
            if op == BinaryOp::Add && f.params[0].ty == cx.ty.str_ && operator_target(cx, owner) {
                return Some(unreachable_concat());
            }
            None
        }
        _ if m.is_static => Some(format!(
            "`{name}` is an instance method: only the binary operators have a static form"
        )),
        OpMethod::Unary(_) => (n != 0).then(|| format!("`{name}` takes no parameters")),
        OpMethod::Assign(_) => {
            if n != 1 {
                Some(format!("`{name}` takes one parameter, the right operand"))
            } else if !returns_void {
                Some(format!("`{name}` changes `this` and returns `void`"))
            } else {
                None
            }
        }
        OpMethod::Index => {
            (n == 0).then(|| format!("`{name}` takes the index (one or more parameters)"))
        }
        OpMethod::SetIndex => {
            if n < 2 {
                Some(format!(
                    "`{name}` takes the index (one or more parameters) and then the value"
                ))
            } else if !returns_void {
                Some(format!("`{name}` changes `this` and returns `void`"))
            } else {
                None
            }
        }
    }
}

fn unreachable_concat() -> String {
    "unreachable operator method: `+` with a `string` operand is always concatenation".into()
}

/// The source spelling of binary operator `op`.
fn op_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
        _ => ">>>",
    }
}

/// `[Symbol.setIndex]`'s value is of the type `[Symbol.index]` returns, so `a[i] op= v` reads
/// and writes one type.
fn index_pair(cx: &mut Ctx, o: &Owner) {
    let def = |name: &str| {
        o.methods
            .iter()
            .find(|(k, m)| k == name && !m.is_static)
            .map(|(_, m)| m.def)
    };
    let (Some(get), Some(set)) = (def(INDEX), def(SET_INDEX)) else {
        return;
    };
    let read = cx.fn_info(get).ret;
    let Some(value) = cx.fn_info(set).params.last().map(|p| p.ty) else {
        return;
    };
    if cx.ty.is_bottom(read) || cx.ty.is_bottom(value) || read == value {
        return;
    }
    let (r, v) = (cx.display(read), cx.display(value));
    let span = cx.fn_info(set).name_span;
    cx.error(
        Diagnostic::error(
            format!("`{SET_INDEX}`'s value must be a `{r}`, what `{INDEX}` returns"),
            span,
        )
        .with_note(format!(
            "it takes a `{v}`: `a[i] op= v` reads the element with `{INDEX}` and writes it back with `{SET_INDEX}`"
        )),
    );
}
