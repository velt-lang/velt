//! A top-level statement that uses a class, an enum or a top-level `let`/`const` declared after
//! it, directly rather than in a function it makes: in JavaScript the declaration has not been
//! evaluated yet when the statement runs, and the use throws a `ReferenceError` (TypeScript
//! rejects it: "used before its declaration", TS2448). Velt evaluates such a declaration where
//! it stands too (#1438), but a class or a constant whose value needs no evaluation would work
//! early, so the use is an error. A use inside a function or arrow runs when it is called and
//! is not checked here (a slot read before its initializer stops with the `ReferenceError` at
//! run time). The parser keeps a variable an earlier statement names at module level
//! (`velt_syntax` `parser::script`), so the use resolves to it here.
//!
//! A semantic error, not a parse error: the file is valid syntax, so `velt fmt` formats it and
//! the editor still shows the module's other diagnostics.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::*;
use velt_syntax::{expr_free_uses, stmt_free_uses, FreeUse};

use crate::ctx::Ctx;

/// Report the early uses of every module's top-level statements (the generated `main`, or the
/// chunks an imported module's statements were split into).
pub(crate) fn check(cx: &mut Ctx) {
    let modules = cx.modules;
    for m in modules {
        let scripts = crate::module_scripts::script_fns(m);
        if scripts.is_empty() {
            continue;
        }
        let mut stmts: Vec<&Stmt> = scripts.iter().flat_map(|f| &f.body.stmts).collect();
        stmts.sort_by_key(|s| s.span.lo);
        let early = early_uses(&m.ast.items, &stmts);
        cx.promoted_consts.extend(early.promoted);
        for (name, what, at) in early.direct {
            let msg = match what {
                "variable" | LOCAL => format!("`{name}` is used before its declaration"),
                _ => format!("{what} `{name}` is used before its declaration"),
            };
            cx.error(Diagnostic::error(msg, at).with_note(
                "top-level statements and declarations run in order: this statement runs before the declaration is evaluated (JavaScript throws a `ReferenceError` here); move the declaration up, or the statement down",
            ));
            if what == LOCAL {
                cx.poisoned.insert((at.file, name));
            }
        }
        for (name, at) in early.local_later {
            cx.error(
                Diagnostic::error(
                    format!("`{name}` is declared below the function that reads it, in a form that stays local to the top-level statements"),
                    at,
                )
                .with_note("a top-level destructuring declaration, a `let` without an initializer or an initializer with `await` cannot be read by a function made above it yet; move the declaration up"),
            );
            cx.poisoned.insert((at.file, name));
        }
    }
}

/// The direct uses of the top-level declarations of `items` in the code before them that runs
/// at once: `stmts` (the module's top-level statements, with the variables the parser moved
/// among them), the initializers of `items`' variables, and their classes' `extends`
/// clauses and static field initializers.
fn early_uses(items: &[Item], stmts: &[&Stmt]) -> Early {
    // (name, what it is, where it starts, the `const`'s declaration)
    let mut decls: Vec<(String, &'static str, u32, Option<Span>)> = vec![];
    for item in items {
        match &item.kind {
            ItemKind::Class(d) => decls.push((d.name.name.clone(), "class", item.span.lo, None)),
            ItemKind::Enum(e) => decls.push((e.name.name.clone(), "enum", item.span.lo, None)),
            ItemKind::Var(v) => {
                let mut names = vec![];
                pattern_names(&v.pattern, &mut names);
                let c = (v.kind == VarKind::Const).then_some(v.span);
                decls.extend(names.into_iter().map(|n| (n, "variable", item.span.lo, c)));
            }
            _ => {}
        }
    }
    // The variables the parser left among the statements (`velt_syntax` `parser::script`).
    for s in stmts {
        if let StmtKind::Var(v) = &s.kind {
            let mut names = vec![];
            pattern_names(&v.pattern, &mut names);
            decls.extend(names.into_iter().map(|n| (n, LOCAL, s.span.lo, None)));
        }
    }
    // The declarations the parser adds (`import.meta`) and the checker declares
    // (`globalThis.<name>`) exist before the module's code runs.
    decls.retain(|(n, ..)| {
        n != velt_syntax::ast::IMPORT_META && !n.starts_with(crate::global_this::PREFIX)
    });
    let mut out = Early::default();
    let Some(last) = decls.iter().map(|(_, _, lo, _)| *lo).max() else {
        return out;
    };
    let mut check = |lo: u32, uses: &dyn Fn() -> Vec<FreeUse>| {
        if lo >= last {
            return;
        }
        for u in uses() {
            let Some((_, what, _, c)) = decls.iter().find(|(n, _, at, _)| *n == u.name && *at > lo)
            else {
                continue;
            };
            match u.deferred {
                true if *what == LOCAL => out.local_later.push((u.name, u.span)),
                true => out.promoted.extend(*c),
                false => out.direct.push((u.name, *what, u.span)),
            }
        }
    };
    for s in stmts {
        check(s.span.lo, &|| stmt_free_uses(s));
    }
    for item in items {
        match &item.kind {
            ItemKind::Var(v) => {
                if let Some(init) = &v.init {
                    check(item.span.lo, &|| expr_free_uses(init));
                }
            }
            ItemKind::Class(d) => {
                if let Some(TypeExpr {
                    kind: TypeExprKind::Named { path, .. },
                    ..
                }) = &d.extends
                {
                    if let [head] = path.as_slice() {
                        check(item.span.lo, &|| {
                            vec![FreeUse {
                                name: head.name.clone(),
                                span: head.span,
                                deferred: false,
                            }]
                        });
                    }
                }
                for f in d.fields.iter().filter(|f| f.is_static) {
                    if let Some(e) = &f.default {
                        check(item.span.lo, &|| expr_free_uses(e));
                    }
                }
            }
            _ => {}
        }
    }
    out.direct.sort_by_key(|(_, _, at)| at.lo);
    out.local_later.sort_by_key(|(_, at)| at.lo);
    out
}

/// What [`early_uses`] found.
#[derive(Default)]
struct Early {
    /// The direct uses: (name, what it is, where it is used).
    direct: Vec<(String, &'static str, Span)>,
    /// The `const`s a function made above them reads: a module state slot each, initialized
    /// where the declaration stands, so a call before it gets Node's `ReferenceError` rather
    /// than the value (a constant is otherwise evaluated where it is used).
    promoted: Vec<Span>,
    /// The uses in a function made above a variable left among the statements, which it cannot
    /// see: (name, where it is used).
    local_later: Vec<(String, Span)>,
}

/// What a variable the parser left among the statements is, in [`early_uses`].
const LOCAL: &str = "local";

fn pattern_names(p: &Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Ident(i) => out.push(i.name.clone()),
        PatternKind::Wildcard => {}
        PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|p| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}
