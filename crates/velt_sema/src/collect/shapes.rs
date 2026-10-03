//! Phase 2: the shape of every type definition — generic bounds, fields (class fields laid out
//! base-first), base classes, `implements` lists, enum variants, interface fields.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ItemDefs;
use crate::ctx::Ctx;
use crate::defs::{Bound, DefInfo, FieldInfo, VariantInfo};
use crate::hir::{AdtKind, DefId, TyId, TyKind};
use crate::resolve::TyEnv;

pub(super) fn resolve_shapes(cx: &mut Ctx, items: &ItemDefs) {
    let mut defs: Vec<DefId> = items.clone();
    defs.sort();
    for &d in &defs {
        match &cx.info[d.0 as usize] {
            DefInfo::Iface(_) => iface_shape(cx, d),
            DefInfo::Adt(_) => adt_shape(cx, d),
            DefInfo::Enum(_) => enum_shape(cx, d),
            _ => {}
        }
    }
    let classes: Vec<DefId> = defs
        .iter()
        .copied()
        .filter(|d| cx.adt(*d).is_some())
        .collect();
    let mut done = vec![false; cx.info.len()];
    for d in classes {
        layout_fields(cx, d, &mut done, &mut vec![]);
    }
    for d in defs {
        check_finite(cx, d);
    }
}

/// Structs and enums are stored inline, so one cannot contain itself by value.
fn check_finite(cx: &mut Ctx, d: DefId) {
    let (name, span, n) = match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) if a.kind != AdtKind::Class => (a.name.clone(), a.span, a.generics.len()),
        DefInfo::Enum(e) => (e.name.clone(), e.span, e.generics.len()),
        _ => return,
    };
    let start = self_type(cx, d, n);
    if contains_by_value(cx, start, d, &mut vec![], 0) {
        cx.error(
            velt_common::Diagnostic::error(format!("recursive type `{name}` has infinite size"), span)
                .with_note("recursive data needs indirection: make it a `class` (heap-allocated) or use an array"),
        );
    }
}

/// Does a value of type `t` (transitively, by value) contain a value of def `target`?
fn contains_by_value(
    cx: &mut Ctx,
    t: TyId,
    target: DefId,
    seen: &mut Vec<TyId>,
    depth: u32,
) -> bool {
    if depth > 32 || seen.contains(&t) {
        return false;
    }
    seen.push(t);
    let parts: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Adt(d, args) => {
            let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                DefInfo::Adt(a) if a.kind != AdtKind::Class => {
                    a.fields.iter().map(|f| f.ty).collect()
                }
                DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
                _ => vec![],
            };
            tys.into_iter().map(|f| cx.ty.subst(f, &args)).collect()
        }
        TyKind::Tuple(ts) => ts,
        TyKind::Option(x) => vec![x],
        TyKind::Result(a, b) => vec![a, b],
        _ => vec![],
    };
    parts.into_iter().any(|p| {
        matches!(cx.ty.kind(p), TyKind::Adt(d, _) if *d == target)
            || contains_by_value(cx, p, target, seen, depth + 1)
    })
}

/// Bounds of generic params `gs` (resolved in `env`, which already names them).
pub(crate) fn resolve_bounds(
    cx: &mut Ctx,
    gs: &[ast::GenericParam],
    env: &TyEnv,
) -> Vec<Vec<Bound>> {
    gs.iter()
        .map(|g| {
            g.bounds
                .iter()
                .filter_map(|b| iface_bound(cx, b, env))
                .collect()
        })
        .collect()
}

/// A type expression that must name an interface (`extends` bound / `implements` entry).
pub(crate) fn iface_bound(cx: &mut Ctx, t: &ast::TypeExpr, env: &TyEnv) -> Option<Bound> {
    let ty = cx.resolve_type(t, env);
    match cx.ty.kind(ty) {
        TyKind::Dyn(iface, args) => Some(Bound {
            iface: *iface,
            args: args.clone(),
        }),
        TyKind::Error => None,
        _ => {
            let tn = cx.display(ty);
            cx.err(format!("`{tn}` is not an interface"), t.span);
            None
        }
    }
}

fn field_info(cx: &mut Ctx, owner: DefId, f: &ast::Field, env: &TyEnv) -> FieldInfo {
    let mut ty = cx.resolve_type(&f.ty, env);
    if f.optional && cx.ty.opt_payload(ty).is_none() && ty != cx.ty.error {
        ty = cx.ty.option(ty);
    }
    if ty == cx.ty.unit {
        cx.err(
            format!("field `{}` cannot have type `void`", f.name.name),
            f.ty.span,
        );
    }
    FieldInfo {
        name: f.name.name.clone(),
        ty,
        span: f.name.span,
        readonly: f.readonly,
        optional: f.optional,
        has_default: f.default.is_some() || f.optional,
        default: None,
        default_throws: vec![],
        private_to: f.is_private.then_some(owner),
    }
}

fn push_field(cx: &mut Ctx, fields: &mut Vec<FieldInfo>, f: FieldInfo) {
    if fields.iter().any(|g| g.name == f.name) {
        cx.err(
            format!("field `{}` is declared more than once", f.name),
            f.span,
        );
        return;
    }
    fields.push(f);
}

fn iface_shape(cx: &mut Ctx, d: DefId) {
    let info = cx.iface(d).expect("ICE: iface");
    let (decl, env) = (
        info.decl.expect("ICE: iface decl"),
        TyEnv::new(info.module, &info.generics.names),
    );
    let bounds = resolve_bounds(cx, &decl.generics, &env);
    let mut fields = vec![];
    for f in &decl.fields {
        let fi = field_info(cx, d, f, &env);
        push_field(cx, &mut fields, fi);
    }
    let parents = decl
        .extends
        .iter()
        .filter_map(|t| iface_bound(cx, t, &env))
        .collect();
    let DefInfo::Iface(i) = &mut cx.info[d.0 as usize] else {
        unreachable!("ICE: iface")
    };
    i.generics.bounds = bounds;
    i.fields = fields;
    i.parents = parents;
}

fn adt_shape(cx: &mut Ctx, d: DefId) {
    let info = cx.adt(d).expect("ICE: adt");
    let (decl, kind) = (info.decl.expect("ICE: adt decl"), info.kind);
    let env = TyEnv::new(info.module, &info.generics.names);
    let bounds = resolve_bounds(cx, &decl.generics, &env);
    let mut fields = vec![];
    for f in decl.fields.iter().filter(|f| !f.is_static) {
        let fi = field_info(cx, d, f, &env);
        push_field(cx, &mut fields, fi);
    }
    let base = decl
        .extends
        .as_ref()
        .and_then(|t| base_class(cx, t, kind, &env));
    let implements = decl
        .implements
        .iter()
        .filter_map(|t| iface_bound(cx, t, &env))
        .collect();
    let a = cx.adt_mut(d);
    a.generics.bounds = bounds;
    a.fields = fields;
    a.base = base;
    a.implements = implements;
}

/// The class an `extends` clause names, if it may be extended.
fn base_class(cx: &mut Ctx, t: &ast::TypeExpr, kind: AdtKind, env: &TyEnv) -> Option<TyId> {
    let bt = cx.resolve_type(t, env);
    if kind != AdtKind::Class {
        cx.err("only classes can `extends` another class", t.span);
        return None;
    }
    let Some((bd, _)) = cx.class_of(bt) else {
        if bt != cx.ty.error {
            let tn = cx.display(bt);
            cx.err(
                format!("`{tn}` is not a class; a class can only extend a class"),
                t.span,
            );
        }
        return None;
    };
    // Their values only come from the runtime (a JSON handle, a record with every key), which a
    // subclass's constructor would bypass.
    for sealed in ["Record", "JsonValue"] {
        if cx.prelude_adt(sealed) == Some(bd) {
            cx.error(
                Diagnostic::error(format!("`{sealed}` cannot be extended"), t.span).with_note(
                    format!("use composition instead: a class with a `{sealed}` field"),
                ),
            );
            return None;
        }
    }
    Some(bt)
}

fn enum_shape(cx: &mut Ctx, d: DefId) {
    let info = cx.enum_info(d).expect("ICE: enum");
    let decl = info.decl.expect("ICE: enum decl");
    let is_string = decl.variants.iter().any(|v| {
        v.discriminant
            .as_ref()
            .is_some_and(|e| const_str(e).is_some())
    });
    let mut variants: Vec<VariantInfo> = vec![];
    let mut next = 0i64;
    for v in &decl.variants {
        let (disc, str_value) = if is_string {
            (variants.len() as i64, string_member(cx, v))
        } else {
            (numeric_member(cx, v, next), None)
        };
        if variants.iter().any(|w| w.name == v.name.name) {
            cx.err(
                format!("variant `{}` is declared more than once", v.name.name),
                v.name.span,
            );
            continue;
        }
        if !is_string && variants.iter().any(|w| w.discriminant == disc) {
            cx.err(
                format!("discriminant value `{disc}` is assigned more than once"),
                v.span,
            );
        }
        variants.push(VariantInfo {
            name: v.name.name.clone(),
            payload: vec![],
            discriminant: disc,
            str_value,
        });
        next = disc.wrapping_add(1);
    }
    let DefInfo::Enum(e) = &mut cx.info[d.0 as usize] else {
        unreachable!("ICE: enum")
    };
    e.variants = variants;
}

/// Discriminant of a member of a numeric enum (`next` when it has no initializer).
fn numeric_member(cx: &mut Ctx, v: &ast::Variant, next: i64) -> i64 {
    match &v.discriminant {
        Some(e) => const_int(e).unwrap_or_else(|| {
            cx.err("enum discriminants must be integer literals", e.span);
            next
        }),
        None => next,
    }
}

/// Value of a member of a string enum: every member needs a string literal initializer.
fn string_member(cx: &mut Ctx, v: &ast::Variant) -> Option<String> {
    let s = v.discriminant.as_ref().and_then(const_str);
    if s.is_none() {
        cx.err(
            format!(
                "member `{}` of a string enum needs a string value (`{} = \"...\"`)",
                v.name.name, v.name.name
            ),
            v.span,
        );
    }
    s
}

/// `"UP"`, `("UP")`.
fn const_str(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        ast::ExprKind::Paren(x) => const_str(x),
        _ => None,
    }
}

/// `5`, `-3`, `(7)`.
fn const_int(e: &ast::Expr) -> Option<i64> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => i64::try_from(*value).ok(),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => const_int(expr).map(|v| -v),
        ast::ExprKind::Paren(x) => const_int(x),
        _ => None,
    }
}

/// Prefix a class's fields with its base class's (substituted) fields, base classes first.
fn layout_fields(cx: &mut Ctx, d: DefId, done: &mut Vec<bool>, stack: &mut Vec<DefId>) {
    if done[d.0 as usize] {
        return;
    }
    let Some(base) = cx.adt(d).and_then(|a| a.base) else {
        done[d.0 as usize] = true;
        return;
    };
    let (bd, bargs) = cx.class_of(base).expect("ICE: base is a class");
    if stack.contains(&bd) || bd == d {
        let span = cx.adt(d).map_or(Span::DUMMY, |a| a.span);
        cx.err("class inheritance cycle", span);
        cx.adt_mut(d).base = None;
        done[d.0 as usize] = true;
        return;
    }
    stack.push(d);
    layout_fields(cx, bd, done, stack);
    stack.pop();
    let inherited: Vec<FieldInfo> = cx.adt(bd).map(|b| b.fields.clone()).unwrap_or_default();
    let mut all = vec![];
    for mut f in inherited {
        f.ty = cx.ty.subst(f.ty, &bargs);
        all.push(f);
    }
    let start = all.len();
    let own = std::mem::take(&mut cx.adt_mut(d).fields);
    for f in own {
        if all.iter().any(|g: &FieldInfo| g.name == f.name) {
            cx.err(
                format!("field `{}` is already declared in a base class", f.name),
                f.span,
            );
            continue;
        }
        all.push(f);
    }
    let a = cx.adt_mut(d);
    a.fields = all;
    a.own_fields_start = start;
    done[d.0 as usize] = true;
}

/// Type of a type definition applied to its own generic params (`Stack<T>` inside `Stack`).
pub(crate) fn self_type(cx: &mut Ctx, d: DefId, n: usize) -> TyId {
    let args = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    cx.ty.intern(TyKind::Adt(d, args))
}
