//! An imported module's top-level statements split into chunks (#1201): in Node a module's
//! body runs in source order, so a statement between two module-level variables runs after
//! the first one's initializer and before the second one's. The parser gathered the
//! statements into one generated `main` (`velt_syntax` `parser::script`); for an imported
//! module, before collection, each run of statements between module-level variables becomes a
//! function of its own (named `#script<N>`, which no source can spell, with an empty name span
//! like the generated `main`), and `module_scripts` gives each one a module state slot at its
//! own position. A variable the parser moved into the generated `main` that a later chunk uses
//! goes back to module level (it splits the statements there too; a `const` becomes a module
//! state slot, not a constant: [`Split::promoted`]). When that is not possible (a destructuring
//! pattern, no initializer) the statements can't run in order: an error.

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::nested_closures::scan::scan_stmt;
use crate::SourceModule;

/// The name prefix of the chunk functions.
pub(crate) const CHUNK: &str = "#script";

/// What [`split`] made.
#[derive(Default)]
pub(crate) struct Split {
    /// The modules with their imported top-level statements split, or `None` when none needs it.
    pub modules: Option<Vec<SourceModule>>,
    /// The spans of the `const` declarations moved back to module level: each is a module state
    /// slot (evaluated once, in order), not a constant evaluated where it is used.
    pub promoted: std::collections::HashSet<Span>,
    pub diags: Vec<Diagnostic>,
}

/// Split the imported modules' top-level statements (module docs).
pub(crate) fn split(modules: &[SourceModule], root: usize) -> Split {
    let mut out = Split::default();
    let splits: Vec<Option<Vec<ast::Item>>> = modules
        .iter()
        .enumerate()
        .map(|(m, module)| {
            (m != root)
                .then(|| split_module(module, &mut out))
                .flatten()
        })
        .collect();
    if splits.iter().all(Option::is_none) {
        return out;
    }
    let split = modules
        .iter()
        .zip(splits)
        .map(|(m, items)| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: ast::Module {
                items: items.unwrap_or_else(|| m.ast.items.clone()),
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    out.modules = Some(split);
    out
}

/// `module`'s items with its generated `main` split into chunks, if it has more than one.
fn split_module(module: &SourceModule, out: &mut Split) -> Option<Vec<ast::Item>> {
    let at = module.ast.items.iter().position(|it| {
        matches!(&it.kind, ast::ItemKind::Function(f)
            if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi)
    })?;
    let ast::ItemKind::Function(script) = &module.ast.items[at].kind else {
        return None;
    };
    if script.sig.is_async {
        return None; // reported by `module_scripts`
    }
    let mut stmts = script.body.stmts.clone();
    stmts.sort_by_key(|s| s.span.lo);
    let refs: Vec<HashSet<String>> = stmts.iter().map(|s| scan_stmt(s).refs).collect();
    // Where a module-level variable splits the statements.
    let mut cuts: Vec<u32> = module
        .ast
        .items
        .iter()
        .filter(|it| matches!(it.kind, ast::ItemKind::Var(_)))
        .map(|it| it.span.lo)
        .collect();
    let mut promoted: HashSet<usize> = HashSet::new();
    let mut failed: HashSet<usize> = HashSet::new();
    let reported = out.diags.len();
    loop {
        let chunk_of = |i: usize| cuts.iter().filter(|&&c| c < stmts[i].span.lo).count();
        let mut more = vec![];
        for (i, s) in stmts.iter().enumerate() {
            let ast::StmtKind::Var(v) = &s.kind else {
                continue;
            };
            if promoted.contains(&i) {
                continue;
            }
            let names = bound(&v.pattern);
            let used_later = (i + 1..stmts.len())
                .any(|j| chunk_of(j) != chunk_of(i) && names.iter().any(|n| refs[j].contains(n)));
            if used_later {
                if !matches!(v.pattern.kind, ast::PatternKind::Ident(_)) || v.init.is_none() {
                    if !failed.insert(i) {
                        continue;
                    }
                    out.diags.push(
                        Diagnostic::error(
                            "this variable is used by top-level statements after a module-level variable, which is not supported yet for a variable declared without an initializer or by destructuring",
                            v.pattern.span,
                        )
                        .with_note("an imported module's statements run in order with its module-level variables, so a variable they share must be a module-level variable itself: declare one variable per name, with an initializer (`const x = obj.x;`)"),
                    );
                    continue;
                }
                more.push(i);
            }
        }
        if more.is_empty() {
            break;
        }
        // Promoting may split a chunk a failed variable's uses share: check again.
        failed.clear();
        out.diags.truncate(reported);
        for i in more {
            cuts.push(stmts[i].span.lo);
            promoted.insert(i);
        }
    }
    if !failed.is_empty() {
        return None;
    }
    let mut chunks: Vec<(usize, Vec<ast::Stmt>)> = vec![];
    let mut vars: Vec<ast::Item> = vec![];
    for (i, s) in stmts.into_iter().enumerate() {
        if promoted.contains(&i) {
            let ast::StmtKind::Var(v) = s.kind else {
                unreachable!("ICE: promoted statement is a variable")
            };
            let span = s.span;
            if v.kind == ast::VarKind::Const {
                out.promoted.insert(v.span);
            }
            vars.push(ast::Item {
                kind: ast::ItemKind::Var(v),
                exported: false,
                span,
            });
            continue;
        }
        let c = cuts.iter().filter(|&&c| c < s.span.lo).count();
        match chunks.last_mut() {
            Some((k, ss)) if *k == c => ss.push(s),
            _ => chunks.push((c, vec![s])),
        }
    }
    if chunks.len() < 2 && promoted.is_empty() {
        return None;
    }
    let mut items = module.ast.items.clone();
    items.remove(at);
    items.extend(vars);
    for (n, (_, ss)) in chunks.into_iter().enumerate() {
        let lo = ss.first().map_or(script.sig.span.lo, |s| s.span.lo);
        let hi = ss.last().map_or(script.sig.span.hi, |s| s.span.hi);
        let span = Span::new(script.sig.span.file, lo, hi);
        let mut sig = script.sig.clone();
        sig.name = ast::Ident {
            name: format!("{CHUNK}{n}"),
            span: Span::new(span.file, lo, lo),
        };
        sig.span = span;
        items.push(ast::Item {
            kind: ast::ItemKind::Function(ast::FnDecl {
                sig,
                body: ast::Block { stmts: ss, span },
                overloads: vec![],
            }),
            exported: false,
            span,
        });
    }
    Some(items)
}

/// The names a declaration pattern binds.
fn bound(p: &ast::Pattern) -> Vec<String> {
    let mut s = crate::nested_closures::scan::Scan::default();
    s.pattern(p);
    s.binds.into_iter().collect()
}
