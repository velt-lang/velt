//! Compile-time decorators (https://github.com/velt-lang/velt/issues/2203). `@x` is compile-time
//! when `x` resolves to a `declare decorator function`, and a TC39 run-time decorator (not
//! supported yet, #2204) when it resolves to anything else. A use is checked against the
//! declaration like a call, with arguments that must be constants ([`consts`]), and recorded
//! for its readers in `Program::decorators`. A program without decorators pays one look per
//! definition here, and nothing later.

mod consts;
mod scope;

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnInfo, FnKind, FnSource};
use crate::hir::{self, ConstTree, DecoratorTarget, DefId};

/// The result types of a `declare decorator function`: where it may be used, with the bit each
/// stands for and how messages say it. They are words only in that position, so they hide no name
/// of a program (a decorator is never called: no value of them exists).
const TARGET_NAMES: [(&str, u8, &str); 5] = [
    ("FunctionDecorator", 1, "a function"),
    ("MethodDecorator", 2, "a method"),
    ("TypeDecorator", 4, "a class, struct or enum"),
    ("FieldDecorator", 8, "a field"),
    ("ParamDecorator", 16, "a parameter"),
];
const FUNCTION: u8 = 1;
const METHOD: u8 = 2;
const TYPE: u8 = 4;
const FIELD: u8 = 8;
const PARAM: u8 = 16;
/// The wrapper in a result type that lets a decorator be used more than once on one target
/// (`Repeatable<FieldDecorator>`).
const REPEATABLE: &str = "Repeatable";

/// Where a decorator may be used and whether it may repeat; `Err` holds what is wrong.
fn targets_of(ty: &ast::TypeExpr) -> Result<(u8, bool), Span> {
    match &ty.kind {
        ast::TypeExprKind::Named { path, args } => match (path.as_slice(), args.as_slice()) {
            ([name], [inner]) if name.name == REPEATABLE => {
                targets_of(inner).map(|(bits, _)| (bits, true))
            }
            ([name], []) => TARGET_NAMES
                .iter()
                .find(|(n, ..)| *n == name.name)
                .map(|(_, bit, _)| (*bit, false))
                .ok_or(ty.span),
            _ => Err(ty.span),
        },
        ast::TypeExprKind::Union(parts) => {
            let mut bits = 0;
            for p in parts {
                let (b, repeat) = targets_of(p)?;
                if repeat {
                    return Err(p.span);
                }
                bits |= b;
            }
            Ok((bits, false))
        }
        _ => Err(ty.span),
    }
}

/// The result type of decorator `sig`, as hover shows it.
pub(crate) fn shown_result(sig: &ast::FnSig) -> String {
    fn show(t: &ast::TypeExpr) -> String {
        match &t.kind {
            ast::TypeExprKind::Named { path, args } => {
                let name: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
                let args: Vec<String> = args.iter().map(show).collect();
                match args.is_empty() {
                    true => name.join("."),
                    false => format!("{}<{}>", name.join("."), args.join(", ")),
                }
            }
            ast::TypeExprKind::Union(parts) => {
                parts.iter().map(show).collect::<Vec<_>>().join(" | ")
            }
            _ => String::new(),
        }
    }
    sig.ret.as_ref().map(show).unwrap_or_default()
}

/// A decorator's declaration in the definition of function `f`, if it is one.
pub(crate) fn decl_of<'m>(f: &FnInfo<'m>) -> Option<&'m ast::FnSig> {
    f.extern_sig.filter(|s| s.is_decorator)
}

/// Check every declaration of a decorator, and every use.
pub(crate) fn check<'m>(cx: &mut Ctx<'m>) {
    let mut seen = FxHashSet::default();
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        match &cx.info[i] {
            DefInfo::Fn(f) => {
                let (module, kind) = (f.module, f.kind);
                let sig: &'m ast::FnSig = match (f.source, f.extern_sig) {
                    (Some(FnSource::Decl(decl)), _) => &decl.sig,
                    (None, Some(sig)) => sig,
                    _ => continue,
                };
                let decorated = !sig.decorators.is_empty()
                    || sig.params.iter().any(|p| !p.decorators.is_empty());
                if sig.is_decorator {
                    check_declaration(cx, module, sig);
                }
                if decorated && seen.insert(sig as *const ast::FnSig as usize) {
                    function_uses(cx, d, module, kind, sig);
                }
            }
            DefInfo::Adt(a) => {
                let (module, Some(decl)) = (a.module, a.decl) else {
                    continue;
                };
                let decorated = !decl.decorators.is_empty()
                    || decl.fields.iter().any(|f| !f.decorators.is_empty());
                if decorated && seen.insert(decl as *const ast::TypeDecl as usize) {
                    type_uses(cx, d, module, decl);
                }
            }
            DefInfo::Enum(e) => {
                let Some(decl) = e.decl.filter(|e| !e.decorators.is_empty()) else {
                    continue;
                };
                let file = decl.name.span.file;
                let Some(module) = cx.modules.iter().position(|m| m.file == file) else {
                    continue;
                };
                let target = DecoratorTarget::Def(d);
                uses(cx, module, &decl.decorators, TYPE, target);
            }
            _ => {}
        }
    }
}

fn function_uses<'m>(cx: &mut Ctx<'m>, d: DefId, m: usize, kind: FnKind, sig: &'m ast::FnSig) {
    let bit = match kind {
        FnKind::Method | FnKind::Static => METHOD,
        _ => FUNCTION,
    };
    uses(cx, m, &sig.decorators, bit, DecoratorTarget::Def(d));
    for (i, p) in sig.params.iter().enumerate() {
        let target = DecoratorTarget::Param(d, i as u32);
        uses(cx, m, &p.decorators, PARAM, target);
    }
}

fn type_uses<'m>(cx: &mut Ctx<'m>, d: DefId, m: usize, decl: &'m ast::TypeDecl) {
    uses(cx, m, &decl.decorators, TYPE, DecoratorTarget::Def(d));
    for f in decl.fields.iter().filter(|f| !f.decorators.is_empty()) {
        let Some(a) = cx.adt(d) else { return };
        let target = match a.statics.get(&f.name.name) {
            Some(g) if f.is_static => DecoratorTarget::Def(*g),
            _ => {
                // An erroneous declaration may have left it out, which is reported already.
                let Some(i) = a.fields.iter().rposition(|x| x.name == f.name.name) else {
                    continue;
                };
                DecoratorTarget::Field(d, i as u32)
            }
        };
        uses(cx, m, &f.decorators, FIELD, target);
    }
}

/// Checks what a declaration says: only a target type makes it a decorator's.
fn check_declaration(cx: &mut Ctx, m: usize, sig: &ast::FnSig) {
    if let Some(g) = sig.generics.first() {
        cx.err("a decorator has no type parameters", g.name.span);
    }
    if sig.is_async {
        cx.err(
            "a decorator is never called, so it can't be `async`",
            sig.name.span,
        );
    }
    let names = || {
        let names: Vec<&str> = TARGET_NAMES.iter().map(|(n, ..)| *n).collect();
        names.join(", ")
    };
    match &sig.ret {
        None => cx.error(
            Diagnostic::error(
                "a decorator's result type says where it may be used",
                sig.name.span,
            )
            .with_note(format!("write one of {} (or a union of them)", names())),
        ),
        Some(ret) => {
            if let Err(span) = targets_of(ret) {
                cx.error(
                    Diagnostic::error("this is not a decorator's result type", span)
                        .with_note(format!("write one of {} (or a union of them), inside `{REPEATABLE}<…>` if it may be used more than once on a target", names())),
                );
            }
        }
    }
    for p in &sig.params {
        let Some(default) = p.default.as_ref().filter(|_| !p.rest) else {
            continue;
        };
        if let Err(e) = consts::value_of(cx, m, default) {
            non_constant(cx, e);
        }
    }
}

fn non_constant(cx: &mut Ctx, e: consts::NotConstant) {
    if let Some((module, name)) = e.type_only {
        cx.type_only_value(module, &name, e.span);
        return;
    }
    cx.error(
        Diagnostic::error("a decorator's argument must be a constant", e.span).with_note(
            "write a literal, an enum member, an array or object literal of constants, or a module-level `const` holding one",
        ),
    );
}

enum Resolved {
    Decorator(DefId, Item),
    RunTime,
    /// Imported with `import type`, under this name.
    TypeOnly(String),
    Unknown(String, Span),
}

/// What decorator expression `path` names in module `m`, where it is written.
fn resolve(cx: &Ctx, m: usize, path: &[ast::Ident]) -> Resolved {
    let (head, last) = match (path.first(), path.last()) {
        (Some(h), Some(l)) => (h, l),
        _ => return Resolved::RunTime,
    };
    // A local of an enclosing function hides any item of that name, and a value that only exists
    // when the code runs is a run-time decorator at best.
    if scope::shadow_of(cx, m, head) == Some(scope::Shadow::Local) {
        return Resolved::RunTime;
    }
    if let Some(name) = scope::type_only_name(cx, m, path) {
        return Resolved::TypeOnly(name);
    }
    match cx.lookup_path_at(m, path, last.span) {
        Some(it @ Item::Def(d)) if cx.try_fn(d).is_some_and(|f| decl_of(f).is_some()) => {
            Resolved::Decorator(d, it)
        }
        Some(_) => Resolved::RunTime,
        None if path.len() > 1 && cx.scopes[m].namespaces.contains_key(&head.name) => {
            let name: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
            Resolved::Unknown(
                format!("cannot find the decorator `{}`", name.join(".")),
                last.span,
            )
        }
        None if path.len() > 1 && cx.lookup_item_at(m, &head.name, head.span).is_some() => {
            Resolved::RunTime
        }
        None => Resolved::Unknown(
            format!("cannot find `{}` in this scope", head.name),
            head.span,
        ),
    }
}

fn run_time_error(cx: &mut Ctx, span: Span, bit: u8) {
    let d = match bit {
        PARAM => Diagnostic::error(
            "parameter decorators are TypeScript's `experimentalDecorators` form, which Velt does not support",
            span,
        )
        .with_note(format!("{} and have no parameter form; a compile-time decorator is declared with `declare decorator function`", ast::TC39_NOT_SUPPORTED)),
        METHOD | FIELD | TYPE => Diagnostic::error(ast::TC39_NOT_SUPPORTED, span).with_note(
            "this is not a `declare decorator function`, so as in TypeScript it would run when the class is defined",
        ),
        _ => Diagnostic::error(ast::TC39_NOT_SUPPORTED, span).with_note(
            "this is not a `declare decorator function`, and a run-time decorator applies only to a class or its members",
        ),
    };
    cx.error(d);
}

/// Checks decorators `ds` and records them for `target`.
fn uses<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    ds: &'m [ast::Decorator],
    bit: u8,
    target: DecoratorTarget,
) {
    let mut out: Vec<hir::Decorator> = Vec::with_capacity(ds.len());
    for dec in ds {
        let (callee, args): (&ast::Expr, &[ast::Expr]) = match &dec.expr.kind {
            ast::ExprKind::Call { callee, args, .. } => (callee, args),
            _ => (&dec.expr, &[]),
        };
        let Some(path) = consts::path_of(callee) else {
            run_time_error(cx, dec.span, bit);
            continue;
        };
        let def = match resolve(cx, m, &path) {
            Resolved::Decorator(def, item) => {
                let last = path.last().map_or(dec.span, |p| p.span);
                cx.rec_item(last, Some(item));
                def
            }
            Resolved::RunTime => {
                run_time_error(cx, dec.span, bit);
                continue;
            }
            Resolved::TypeOnly(name) => {
                cx.type_only_value(m, &name, path[0].span);
                continue;
            }
            Resolved::Unknown(message, span) => {
                let name = path.last().map_or("", |p| p.name.as_str()).to_string();
                cx.not_found(message, &name, span);
                continue;
            }
        };
        let name = cx.fn_info(def).name.clone();
        let Some(sig) = decl_of(cx.fn_info(def)) else {
            continue;
        };
        let declared = sig.ret.as_ref().and_then(|r| targets_of(r).ok());
        let (bits, repeatable) = declared.unwrap_or((u8::MAX, true));
        if bits & bit == 0 {
            let wanted: Vec<&str> = TARGET_NAMES
                .iter()
                .filter(|(_, b, _)| bits & b != 0)
                .map(|(.., what)| *what)
                .collect();
            let here = TARGET_NAMES.iter().find(|(_, b, _)| *b == bit);
            let here = here.map_or("this", |(.., what)| what);
            cx.error(
                Diagnostic::error(
                    format!("the decorator `{name}` cannot be applied to {here}"),
                    dec.span,
                )
                .with_note(format!("it applies to {}", wanted.join(", "))),
            );
            continue;
        }
        if let Some(first) = out.iter().find(|u| u.def == def).filter(|_| !repeatable) {
            let first = first.span;
            cx.error(
                Diagnostic::error(
                    format!("the decorator `{name}` is applied more than once"),
                    dec.span,
                )
                .with_label(first, "first applied here")
                .with_note(
                    "its declaration may allow this: `Repeatable<…>` around its result type",
                ),
            );
            continue;
        }
        if let Some(args) = arguments(cx, m, def, &name, sig, args, dec.span) {
            out.push(hir::Decorator {
                def,
                args,
                span: dec.span,
            });
        }
    }
    if !out.is_empty() {
        cx.decorator_uses.insert(target, out);
    }
}

/// The value of each parameter of decorator `def` in a use with arguments `args`.
fn arguments(
    cx: &mut Ctx,
    m: usize,
    def: DefId,
    name: &str,
    sig: &ast::FnSig,
    args: &[ast::Expr],
    at: Span,
) -> Option<Vec<ConstTree>> {
    let decl_module = cx.fn_info(def).module;
    let tys: Vec<_> = cx.fn_info(def).params.iter().map(|p| p.ty).collect();
    if args.len() > sig.params.len() && !sig.params.last().is_some_and(|p| p.rest) {
        cx.err(
            format!(
                "the decorator `{name}` takes {} argument(s), but {} were given",
                sig.params.len(),
                args.len()
            ),
            args[sig.params.len()].span,
        );
        return None;
    }
    let mut out = Vec::with_capacity(sig.params.len());
    let mut ok = true;
    for (i, (p, &ty)) in sig.params.iter().zip(&tys).enumerate() {
        if p.rest {
            let mut items = vec![];
            for a in args.get(i..).unwrap_or_default() {
                match checked_value(cx, m, a, ty, true) {
                    Some(v) => items.push(v),
                    None => ok = false,
                }
            }
            out.push(ConstTree::Array(items));
        } else if let Some(a) = args.get(i) {
            match checked_value(cx, m, a, ty, false) {
                Some(v) => out.push(v),
                None => ok = false,
            }
        } else if let Some(default) = &p.default {
            // A default that isn't constant is reported at the declaration.
            match consts::value_of(cx, decl_module, default) {
                Ok(v) => out.push(v),
                Err(_) => ok = false,
            }
        } else {
            cx.err(
                format!(
                    "the decorator `{name}` is missing the argument `{}`",
                    p.name.name
                ),
                at,
            );
            ok = false;
        }
    }
    ok.then_some(out)
}

/// The value of argument `a`, reported when it isn't constant or doesn't fit `ty` (`rest`: an
/// array type whose elements it is one of).
fn checked_value(
    cx: &mut Ctx,
    m: usize,
    a: &ast::Expr,
    ty: hir::TyId,
    rest: bool,
) -> Option<ConstTree> {
    let v = match consts::value_of(cx, m, a) {
        Ok(v) => v,
        Err(e) => {
            non_constant(cx, e);
            return None;
        }
    };
    let want = match (rest, cx.ty.kind(ty)) {
        (true, hir::TyKind::Array(elem)) => *elem,
        _ => ty,
    };
    if consts::fits(cx, &v, want) {
        return Some(v);
    }
    let shown = cx.display(want);
    cx.err(
        format!("this argument of the decorator doesn't fit its parameter's type `{shown}`"),
        a.span,
    );
    None
}
