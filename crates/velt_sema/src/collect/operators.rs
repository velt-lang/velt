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
//!
//! A type with operator methods implements their `velt/ops` interfaces without an `implements`
//! clause ([`impls`]), so generic code bounded by them uses the operators.

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
    /// The number of its type parameters (the class's or the block's).
    generics: u32,
    methods: Vec<(String, MethodRef)>,
    /// An `extend` block, which user code may not write for a type with built-in operators.
    extend: bool,
}

pub(super) fn check(cx: &mut Ctx) {
    misplaced_keys(cx);
    let owners = owners(cx);
    for o in &owners {
        check_owner(cx, o);
    }
    impls(cx, &owners);
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
                generics: n as u32,
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
                generics: x.generics.len() as u32,
                methods: ms,
                extend: true,
            });
        }
    }
    owners
}

/// A setter or a field keyed by an operator's symbol (`set [Symbol.add](v)`,
/// `[Symbol.mul]: number = 3`): never what the operator calls.
fn misplaced_keys(cx: &mut Ctx) {
    let mut errs = vec![];
    for i in 0..cx.info.len() {
        let Some(a) = cx.adt(DefId(i as u32)) else {
            continue;
        };
        for (k, m) in &a.methods {
            let Some(base) = k.strip_prefix("set ").filter(|b| b.starts_with('[')) else {
                continue;
            };
            if op_method(base).is_some() {
                errs.push((base.to_string(), cx.fn_info(m.def).name_span, "an accessor"));
            }
        }
        for f in &a.fields[a.own_fields_start..] {
            if f.name.starts_with('[') && op_method(&f.name).is_some() {
                errs.push((f.name.clone(), f.span, "a field"));
            }
        }
    }
    for (name, span, what) in errs {
        cx.error(Diagnostic::error(
            format!("`{name}` names an operator method: it cannot be {what}"),
            span,
        ));
    }
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
            if op == BinaryOp::Add && cx.string_like(left) {
                return Some(unreachable_concat());
            }
            if right != owner && !cx.ty.is_bottom(right) {
                let (shown, l, r) = (cx.display(owner), cx.display(left), cx.display(right));
                return Some(format!(
                    "a static `{name}` is found on the type of its right operand, `{shown}`: its second parameter has that type, not `{r}` (`static {name}(a: {l}, b: {shown})`)"
                ));
            }
            None
        }
        OpMethod::Binary(op) => {
            if n != 1 {
                return Some(format!("`{name}` takes one parameter, the right operand"));
            }
            // std's `extend string` gives `string` its `+` for generic code (`velt/ops`).
            if op == BinaryOp::Add && cx.string_like(f.params[0].ty) && operator_target(cx, owner) {
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

/// The `velt/ops` interface named `name`, if the module is loaded.
fn ops_iface(cx: &Ctx, name: &str) -> Option<DefId> {
    let module = velt_common::std_item!(ops_operators, "");
    match cx.std_def(&format!("{module}{name}"))? {
        crate::ctx::Item::Def(d) if cx.iface(d).is_some() => Some(d),
        _ => None,
    }
}

/// The `velt/ops` interface and its arguments that operator method `m` (named `name`)
/// implements: `Add<R, O>` for `[Symbol.add](other: R): O`, `Neg<O>`, `AddAssign<R>`,
/// `Index<K, V>` and `SetIndex<K, V>` (one index).
fn implemented(cx: &Ctx, name: &str, m: &MethodRef) -> Option<(&'static str, Vec<TyId>)> {
    let f = cx.fn_info(m.def);
    let p = |i: usize| f.params.get(i).map(|p| p.ty);
    let iface = |key: &str| -> Option<&'static str> {
        let k = key.strip_prefix("[Symbol.")?.strip_suffix(']')?;
        IFACES.iter().find(|(kk, _)| *kk == k).map(|(_, i)| *i)
    };
    let name_of = iface(name)?;
    let args = match op_method(name)? {
        OpMethod::Binary(_) if f.params.len() == 1 => vec![p(0)?, f.ret],
        OpMethod::Unary(_) if f.params.is_empty() => vec![f.ret],
        OpMethod::Assign(_) if f.params.len() == 1 => vec![p(0)?],
        OpMethod::Index if f.params.len() == 1 => vec![p(0)?, f.ret],
        OpMethod::SetIndex if f.params.len() == 2 => vec![p(0)?, p(1)?],
        _ => return None,
    };
    Some((name_of, args))
}

/// The interface of each operator key.
const IFACES: [(&str, &str); 28] = [
    ("add", "Add"),
    ("sub", "Sub"),
    ("mul", "Mul"),
    ("div", "Div"),
    ("rem", "Rem"),
    ("pow", "Pow"),
    ("bitAnd", "BitAnd"),
    ("bitOr", "BitOr"),
    ("bitXor", "BitXor"),
    ("shl", "Shl"),
    ("shr", "Shr"),
    ("ushr", "UShr"),
    ("neg", "Neg"),
    ("bitNot", "BitNot"),
    ("addAssign", "AddAssign"),
    ("subAssign", "SubAssign"),
    ("mulAssign", "MulAssign"),
    ("divAssign", "DivAssign"),
    ("remAssign", "RemAssign"),
    ("powAssign", "PowAssign"),
    ("bitAndAssign", "BitAndAssign"),
    ("bitOrAssign", "BitOrAssign"),
    ("bitXorAssign", "BitXorAssign"),
    ("shlAssign", "ShlAssign"),
    ("shrAssign", "ShrAssign"),
    ("ushrAssign", "UShrAssign"),
    ("index", "Index"),
    ("setIndex", "SetIndex"),
];

/// A type with an operator method implements its `velt/ops` interface without an `implements`
/// clause (as `compareTo` makes a type `Comparable`), so generic code bounded by it uses the
/// operator: an entry in `Program::impls` per method (per overload signature), and
/// `Arithmetic<T>` where `T` has `+`, `-`, `*`, `/` and unary `-` on itself. Nothing when the
/// program does not load `velt/ops`.
fn impls(cx: &mut Ctx, owners: &[Owner]) {
    if ops_iface(cx, "Add").is_none() {
        return;
    }
    for o in owners {
        let mut own: Vec<(&str, DefId, Vec<TyId>, DefId)> = vec![];
        for (name, m) in o.methods.iter().filter(|(_, m)| !m.is_static) {
            let Some((iname, args)) = implemented(cx, name, m) else {
                continue;
            };
            let Some(iface) = ops_iface(cx, iname) else {
                continue;
            };
            own.push((name, iface, args.clone(), m.def));
            push_impl(cx, o, iface, args, vec![m.def]);
        }
        arithmetic(cx, o, &own);
    }
}

/// `ImplDef` of `iface<args>` for `o`'s type with `methods`, unless it has one already.
fn push_impl(cx: &mut Ctx, o: &Owner, iface: DefId, args: Vec<TyId>, methods: Vec<DefId>) {
    let taken = cx
        .impls
        .iter()
        .any(|x| x.iface == iface && x.ty == o.ty && x.iface_args == args);
    if taken {
        return;
    }
    // Called through `Callee::ParamMethod`: the interface's borrow ABI.
    for &m in &methods {
        cx.fn_info_mut(m).fixed_modes = true;
    }
    cx.impls.push(crate::hir::ImplDef {
        ty: o.ty,
        generics: o.generics,
        iface,
        iface_args: args,
        methods,
    });
}

/// `Arithmetic<T>` for `o`'s type `T` when its own operator methods (`own`: key, interface,
/// args, method) give `Add<T, T>`, `Sub<T, T>`, `Mul<T, T>`, `Div<T, T>` and `Neg<T>`: the methods
/// in the interface's slot order.
fn arithmetic(cx: &mut Ctx, o: &Owner, own: &[(&str, DefId, Vec<TyId>, DefId)]) {
    let Some(arith) = ops_iface(cx, "Arithmetic") else {
        return;
    };
    let t = o.ty;
    let slots: Vec<String> = match cx.iface(arith) {
        Some(i) => i.methods.iter().map(|m| m.name.clone()).collect(),
        None => return,
    };
    let mut methods = vec![];
    for key in &slots {
        let found = own.iter().find(|(name, iface, args, _)| {
            let unary = cx.iface(*iface).is_some_and(|i| i.name == "Neg");
            name == key && args.iter().all(|a| *a == t) && (unary || args.len() == 2)
        });
        match found {
            Some((.., m)) => methods.push(*m),
            None => return,
        }
    }
    push_impl(cx, o, arith, vec![t], methods);
}
