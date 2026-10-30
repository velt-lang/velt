//! Script variables that can't be module state go back into `main` (#1872). In the root file's
//! top-level statements (`velt_syntax` `parser::script`), the parser keeps a top-level variable
//! at module level, as module state, when a function the code makes refers to it and the code
//! assigns it: as in Node, the function sees the module's variable. The parser can't see types,
//! so such a variable may be one that module state can't hold: a promise (`let p =
//! Promise.resolve(1)`, `body::driver::promise_in_state`), or one that a `Mutex` `with` callback
//! uses (`module_state::check_locked_access`). Those checks record the variable
//! (`Ctx::state_refused`); when one is a variable of the root file's statements that only they
//! use, the program is checked again with it moved into the generated `main` as a local, where
//! it was before #1872 (closures capture it). A variable a declaration, an export or a statement
//! above it uses stays at module level, and the error stands.

use rustc_hash::FxHashSet;

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir;
use crate::SourceModule;
use velt_common::{Diagnostics, Span};

/// What one check returns: the program, the diagnostics, and the names of the root module's
/// variables that module state refused ([`refused`]).
pub(crate) type Checked = ((Option<hir::Program>, Diagnostics), Vec<String>);

/// The names of the root module's variables that module state refused in `cx`, when it found
/// errors.
pub(crate) fn refused(cx: &Ctx, root: usize) -> Vec<String> {
    if crate::error_count(cx) == 0 {
        return vec![];
    }
    (cx.state_refused.iter())
        .filter_map(|d| match cx.info.get(d.0 as usize) {
            Some(DefInfo::Global(g)) if g.module == root => Some(g.name.clone()),
            _ => None,
        })
        .collect()
}

/// Check `modules` with `check`, again with the refused script variables moved into `main` as
/// long as that moves more of them (module docs).
pub(crate) fn check<T>(
    modules: &[SourceModule],
    root: usize,
    mut check: impl FnMut(&[SourceModule]) -> (T, Vec<String>),
) -> T {
    let mut moved: FxHashSet<String> = FxHashSet::default();
    loop {
        let rewritten = (!moved.is_empty())
            .then(|| with_moved(modules, root, &moved))
            .flatten();
        let checked = rewritten.as_deref().unwrap_or(modules);
        let (out, refused) = check(checked);
        let Some(module) = checked.get(root) else {
            return out;
        };
        let more: Vec<String> = (refused.into_iter())
            .filter(|n| !moved.contains(n) && movable(&module.ast, n))
            .collect();
        if more.is_empty() {
            return out;
        }
        moved.extend(more);
    }
}

/// The generated `main` of a root file's statements: its index among `items`.
fn script_main(items: &[ast::Item]) -> Option<usize> {
    items.iter().position(|it| {
        matches!(&it.kind, ast::ItemKind::Function(f) if velt_syntax::is_script_main(&f.sig.name))
    })
}

/// The top-level variable `name` declares (alone, by a plain name) and its index.
fn var_named<'a>(items: &'a [ast::Item], name: &str) -> Option<(usize, &'a ast::Item)> {
    items.iter().enumerate().find(|(_, it)| {
        matches!(&it.kind, ast::ItemKind::Var(v)
            if matches!(&v.pattern.kind, ast::PatternKind::Ident(i) if i.name == name))
    })
}

/// Can variable `name` of the root module `m` move into its generated `main`: a script's own
/// variable, which no declaration, export or statement above it uses?
fn movable(m: &ast::Module, name: &str) -> bool {
    let Some(at) = script_main(&m.items) else {
        return false;
    };
    let Some((i, var)) = var_named(&m.items, name) else {
        return false;
    };
    let ast::ItemKind::Function(main) = &m.items[at].kind else {
        return false;
    };
    let listed = m.items.iter().any(|it| {
        matches!(&it.kind, ast::ItemKind::Import(imp)
            if imp.from.is_empty() && imp.names.iter().any(|n| n.name.name == name))
    });
    let declared_use = (m.items.iter().enumerate())
        .any(|(j, it)| j != i && j != at && velt_syntax::item_free_names(it).contains(name));
    let above = (main.body.stmts.iter())
        .filter(|s| s.span.lo < var.span.lo)
        .any(|s| {
            velt_syntax::stmt_free_uses(s)
                .iter()
                .any(|u| u.name == name)
        });
    !var.exported && !listed && !declared_use && !above
}

/// `modules` with the root module's variables `names` moved into its generated `main`, in
/// source order with its statements.
fn with_moved(
    modules: &[SourceModule],
    root: usize,
    names: &FxHashSet<String>,
) -> Option<Vec<SourceModule>> {
    let m = &modules.get(root)?.ast;
    let mut items = m.items.clone();
    let mut vars = vec![];
    for name in names {
        if let Some((i, _)) = var_named(&items, name) {
            vars.push(items.remove(i));
        }
    }
    let at = script_main(&items)?;
    let ast::ItemKind::Function(main) = &mut items[at].kind else {
        return None;
    };
    for var in vars {
        let ast::ItemKind::Var(v) = var.kind else {
            continue;
        };
        let lo = var.span.lo.min(main.body.span.lo);
        main.body.span = Span::new(main.body.span.file, lo, main.body.span.hi);
        main.sig.span = main.body.span;
        main.body.stmts.push(ast::Stmt {
            kind: ast::StmtKind::Var(v),
            span: var.span,
        });
    }
    main.body.stmts.sort_by_key(|s| s.span.lo);
    let span = main.body.span;
    items[at].span = span;
    Some(
        (modules.iter().enumerate())
            .map(|(k, s)| SourceModule {
                path: s.path.clone(),
                is_std: s.is_std,
                file: s.file,
                ast: ast::Module {
                    items: if k == root {
                        items.clone()
                    } else {
                        s.ast.items.clone()
                    },
                    span: s.ast.span,
                    jsx_import_source: s.ast.jsx_import_source.clone(),
                },
                imports: s.imports.clone(),
                jsx_runtime: s.jsx_runtime.clone(),
            })
            .collect(),
    )
}
