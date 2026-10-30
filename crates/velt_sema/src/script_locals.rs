//! Script variables that can't be module state go back into `main` (#1872). In the root file's
//! top-level statements (`velt_syntax` `parser::script`), the parser keeps a top-level variable
//! at module level, as module state, when a function the code makes refers to it and the code
//! assigns it: as in Node, the function sees the module's variable. The parser can't see types,
//! so such a variable may be one that module state can't hold: a promise (`let p =
//! Promise.resolve(1)`, `body::driver::promise_in_state`), or one that a `Mutex` `with` callback
//! uses (`module_state::check_locked_access`). Those checks record the variable
//! (`Ctx::state_refused`: all of them, every variable a refused `with` callback reaches too;
//! one that calls a function value reaches those of every closure the code around it makes).
//! Those that are variables of the root file's statements that only they use move into the
//! generated `main` as locals, where they were before #1872 (closures capture them), with the
//! variables below whose initializers use them, and the program is checked again: one extra
//! check. A variable a declaration, an export or a statement above it uses stays at module
//! level, and the error stands.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir;
use crate::SourceModule;
use velt_common::{Diagnostics, Span};

/// What one check returns: the program, the diagnostics, and the variables module state
/// refused ([`refused`]).
pub(crate) type Checked = ((Option<hir::Program>, Diagnostics), Refused);

/// The root module's variables that module state refused in a check.
#[derive(Default)]
pub(crate) struct Refused {
    /// Their names.
    pub vars: Vec<String>,
}

/// The root module's variables that module state refused in `cx`, when it found errors.
pub(crate) fn refused(cx: &Ctx, root: usize) -> Refused {
    if crate::error_count(cx) == 0 {
        return Refused::default();
    }
    let vars = (cx.state_refused.iter())
        .filter_map(|d| match cx.info.get(d.0 as usize) {
            Some(DefInfo::Global(g)) if g.module == root => Some(g.name.clone()),
            _ => None,
        })
        .collect();
    Refused { vars }
}

/// Check `modules` with `check`, and again with the refused script variables that can move
/// moved into `main`: one extra check, since a check reports every variable it refuses and
/// [`movable`] moves them together (module docs). It checks again only while that moves more.
pub(crate) fn check<T>(
    modules: &[SourceModule],
    root: usize,
    mut check: impl FnMut(&[SourceModule]) -> (T, Refused),
) -> T {
    let mut moved: FxHashSet<String> = FxHashSet::default();
    loop {
        let rewritten = (!moved.is_empty())
            .then(|| with_moved(modules, root, &moved))
            .flatten();
        let (out, refused) = check(rewritten.as_deref().unwrap_or(modules));
        let Some(module) = modules.get(root) else {
            return out;
        };
        let candidates = moved.iter().cloned().chain(refused.vars).collect();
        let more = movable(&module.ast, candidates);
        if more.is_subset(&moved) {
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
    items
        .iter()
        .enumerate()
        .find(|(_, it)| var_name(it) == Some(name))
}

/// The name a top-level variable declares, when it declares one by a plain name.
fn var_name(it: &ast::Item) -> Option<&str> {
    match &it.kind {
        ast::ItemKind::Var(v) => match &v.pattern.kind {
            ast::PatternKind::Ident(i) => Some(&i.name),
            _ => None,
        },
        _ => None,
    }
}

/// Which of the `candidates`, variables of the root module `m`, can move into its generated
/// `main`, together with the variables below them whose initializers use them (a module-level
/// initializer can't see a local of `main`; in `main` it runs where it stands, after them, as
/// before): a script's own variables that no declaration, export, statement above them or
/// variable that stays uses. To a fixed point, so a use from a variable that moves too doesn't
/// keep one (`let q = p;`, `let ok = p !== undefined;`, `let f = () => p;`).
fn movable(m: &ast::Module, candidates: FxHashSet<String>) -> FxHashSet<String> {
    let none = FxHashSet::default();
    let Some(at) = script_main(&m.items) else {
        return none;
    };
    let ast::ItemKind::Function(main) = &m.items[at].kind else {
        return none;
    };
    let stmt_uses: Vec<_> = (main.body.stmts.iter())
        .map(|s| (s.span.lo, velt_syntax::stmt_free_uses(s)))
        .collect();
    // A script's own variable: not exported, not listed by an import, used by no statement
    // above it.
    let own = |name: &str, var: &ast::Item| {
        let ast::ItemKind::Var(_) = &var.kind else {
            return false;
        };
        let listed = m.items.iter().any(|it| {
            matches!(&it.kind, ast::ItemKind::Import(imp)
                if imp.from.is_empty() && imp.names.iter().any(|n| n.name.name == name))
        });
        let above = (stmt_uses.iter())
            .any(|(lo, uses)| *lo < var.span.lo && uses.iter().any(|u| u.name == name));
        !var.exported && !listed && !above
    };
    let free: Vec<FxHashSet<String>> = (m.items.iter().enumerate())
        .map(|(j, it)| match j == at {
            true => FxHashSet::default(),
            false => velt_syntax::item_free_names(it),
        })
        .collect();
    let mut set: FxHashMap<String, usize> = FxHashMap::default();
    let mut todo: Vec<String> = candidates.into_iter().collect();
    while let Some(name) = todo.pop() {
        let Some((i, var)) = var_named(&m.items, &name).filter(|(_, v)| own(&name, v)) else {
            continue;
        };
        if set.insert(name.clone(), i).is_some() {
            continue;
        }
        // The variables below it whose initializers use it go too.
        for (j, it) in m.items.iter().enumerate() {
            if it.span.lo > var.span.lo && free[j].contains(&name) {
                todo.extend(
                    var_name(it)
                        .filter(|n| !set.contains_key(*n))
                        .map(String::from),
                );
            }
        }
    }
    loop {
        let kept: Vec<String> = (set.iter())
            .filter(|(name, &i)| {
                (m.items.iter().enumerate()).any(|(j, it)| {
                    j != i && free[j].contains(*name) && !moves_too(it, &m.items[i], &set)
                })
            })
            .map(|(name, _)| name.clone())
            .collect();
        if kept.is_empty() {
            return set.into_keys().collect();
        }
        for name in kept {
            set.remove(&name);
        }
    }
}

/// Is `it`, an item that uses the variable `var`, a variable below it among those that move?
fn moves_too(it: &ast::Item, var: &ast::Item, moving: &FxHashMap<String, usize>) -> bool {
    it.span.lo > var.span.lo && var_name(it).is_some_and(|n| moving.contains_key(n))
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
                is_dependency: s.is_dependency,
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

#[cfg(test)]
#[path = "script_locals_tests.rs"]
mod sema_tests;

#[cfg(test)]
mod tests {
    use velt_common::FileId;

    use super::*;

    /// Run [`check`] over `src` with a stand-in for sema that refuses every variable still at
    /// module level whose name starts with one of `refused`, as sema reports every variable
    /// module state refuses. The number of checks, and the variables moved into `main`.
    fn checks(src: &str, refused: &[&str]) -> (usize, Vec<String>) {
        let (ast, d) = velt_syntax::parse_file(FileId(0), src);
        assert!(d.is_empty(), "{d:?}");
        let m = SourceModule {
            path: "main".into(),
            is_std: false,
            is_dependency: false,
            file: FileId(0),
            ast,
            imports: vec![],
            jsx_runtime: None,
        };
        let originally: Vec<String> = (m.ast.items.iter())
            .filter_map(var_name)
            .map(String::from)
            .collect();
        let mut count = 0;
        let moved = check(&[m], 0, |ms| {
            count += 1;
            let items = &ms[0].ast.items;
            let stmts = script_main(items).map_or(&[][..], |at| match &items[at].kind {
                ast::ItemKind::Function(f) => &f.body.stmts[..],
                _ => &[],
            });
            let local = |n: &String| {
                stmts.iter().any(|s| {
                    matches!(&s.kind, ast::StmtKind::Var(v)
                        if matches!(&v.pattern.kind, ast::PatternKind::Ident(i) if i.name == *n))
                })
            };
            let mut locals: Vec<String> = originally.iter().filter(|n| local(n)).cloned().collect();
            locals.sort();
            let vars = (items.iter().filter_map(var_name))
                .filter(|n| refused.iter().any(|r| n.starts_with(r)))
                .map(String::from)
                .collect();
            (locals, Refused { vars })
        });
        (count, moved)
    }

    /// A chain of promise `let`s a closure assigns, and many variables one `with` callback
    /// reads: one extra check moves them all, however many there are.
    #[test]
    fn every_refused_variable_moves_in_one_extra_check() {
        let mut chain = String::from("let q0 = Promise.resolve(1);\n");
        let mut bumped = String::new();
        for i in 1..40 {
            chain += &format!(
                "let q{i} = q{};\nconst f{i} = () => {{ q{i} = q0; }};\n",
                i - 1
            );
            bumped += &format!("let v{i} = 0;\nconst b{i} = () => {{ v{i}++; }};\n");
        }
        let (n, moved) = checks(&format!("{chain}{bumped}console.log(1);"), &["q", "v"]);
        assert_eq!((n, moved.len()), (2, 79), "{moved:?}");
    }

    /// A variable that stays module state for a closure and reads the refused one in its
    /// initializer moves with it: its initializer can't see a local of `main`.
    #[test]
    fn a_later_initializer_reading_it_moves_along() {
        let src = "let p = Promise.resolve(1);
            const f = () => { p = Promise.resolve(2); };
            let ok = p !== undefined;
            const g = () => { ok = false; };
            let later = () => p;
            const h = () => { later = () => p; };
            console.log(ok);";
        let (n, moved) = checks(src, &["p"]);
        assert_eq!(
            (n, moved),
            (2, vec!["later".into(), "ok".into(), "p".into()])
        );
    }

    /// A declaration that uses it keeps it at module level, and the variables that read it.
    #[test]
    fn a_declaration_keeps_it() {
        let src = "let p = Promise.resolve(1);
            const f = () => { p = Promise.resolve(2); };
            let ok = p !== undefined;
            const g = () => { ok = false; };
            function read() { return ok; }
            console.log(ok);";
        assert_eq!(checks(src, &["p"]), (1, vec![]));
    }
}
