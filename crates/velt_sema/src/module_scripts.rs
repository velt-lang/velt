//! Top-level statements of imported modules (#1201). The parser turns a module's top-level
//! statements into a generated `main` whose name has an empty span (`velt_syntax`
//! `parser::script`). In the root file that is the program's `main`; in an imported module it
//! runs once when the module is evaluated, as in Node: it (or each chunk `module_chunks` split
//! it into) becomes a slot of the module state (`module_state`), of type `void`, whose
//! initializer calls it. The slot is ordered with the module's other slots by where its
//! statements start, and the modules in Node's evaluation order, so the statements run after
//! the modules the module imports and before the modules that import it. A module that is only
//! imported for its types (`import type`) is never evaluated: its statements don't run
//! (`module_state::order`). The statements may not start asynchronous work yet: they run
//! before the scheduler starts (#846). In the root file, a promise statement is started and
//! waited for as in Node ([`is_root_script`]).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::{self, Callee, DefId, ExprKind as E};
use crate::SourceModule;

/// The functions holding module `m`'s top-level statements: the generated `main`, or the
/// chunks `module_chunks` split it into (names with an empty span).
fn script_fns(m: &SourceModule) -> Vec<&ast::FnDecl> {
    m.ast
        .items
        .iter()
        .filter_map(|it| match &it.kind {
            ast::ItemKind::Function(f)
                if (f.sig.name.name == "main"
                    || f.sig.name.name.starts_with(crate::module_chunks::CHUNK))
                    && f.sig.name.span.lo == f.sig.name.span.hi =>
            {
                Some(f)
            }
            _ => None,
        })
        .collect()
}

/// Does module `m` hold the root file's top-level statements, in its generated `main`? There a
/// promise statement (`renderAll();`) runs as in Node: started at once and waited for before the
/// program exits, as a dropped promise is (`body::expr::tasks::check_floating`).
pub(crate) fn is_root_script(cx: &Ctx, m: usize) -> bool {
    m == cx.root
        && cx
            .modules
            .get(m)
            .is_some_and(|module| script_fns(module).iter().any(|f| f.sig.name.name == "main"))
}

/// Give each imported module's top-level statements a module state slot (module docs); after
/// the bodies are checked, so the generated function's result type is known.
pub(crate) fn declare(cx: &mut Ctx) {
    let (modules, root) = (cx.modules, cx.root);
    for (m, module) in modules.iter().enumerate() {
        if m == root {
            continue;
        }
        for script in script_fns(module) {
            declare_one(cx, m, script);
        }
    }
}

fn declare_one(cx: &mut Ctx, m: usize, script: &ast::FnDecl) {
    {
        let span = script.sig.span;
        let Some(Item::Def(f)) = cx.scopes[m].items.get(&script.sig.name.name).copied() else {
            return;
        };
        let Some(DefInfo::Fn(info)) = cx.info.get(f.0 as usize) else {
            return;
        };
        let (is_async, ret) = (info.is_async, info.ret);
        if is_async {
            cx.error(
                Diagnostic::error(
                    "top-level `await` in an imported module is not supported yet",
                    span,
                )
                .with_note("only the file the program starts from may `await` at the top level; put the code in an `async` function and call it from there"),
            );
            return;
        }
        let work = async_work(cx, f);
        for (at, what) in &work {
            cx.error(
                Diagnostic::error(
                    format!("an imported module's top-level statements cannot start asynchronous work yet ({what})"),
                    *at,
                )
                .with_note("they run while the modules are evaluated, before the scheduler starts: a timer would fire out of Node's order, and a promise callback would never run (#846)")
                .with_note("start it from a function that the program calls"),
            );
        }
        if !work.is_empty() {
            return;
        }
        let call = hir::Expr {
            kind: E::Call {
                callee: Callee::Def(f, vec![]),
                args: vec![],
            },
            ty: ret,
            span,
        };
        let name = "<top-level statements>".to_string();
        let global = GlobalInfo {
            qual_name: cx.qualify(m, &name),
            name,
            module: m,
            span,
            ty: ret,
            init: Some(call),
            state: BodyState::Done,
            src: GlobalSrc {
                ann: None,
                init: None,
                span,
                owner: None,
                is_private: false,
            },
            mutable: false,
            stored: true,
        };
        let g = cx.alloc_def(span, DefInfo::Global(Box::new(global)));
        cx.module_scripts.push(g);
    }
}

/// Where the body of `f` (not the closures it makes) starts asynchronous work, outermost
/// expressions only: a timer, a microtask, a task, or a promise (which only a `then` chain kept
/// as a statement leaves unawaited in synchronous code); with what it is.
fn async_work(cx: &Ctx, f: DefId) -> Vec<(velt_common::Span, String)> {
    let Some(Some(hir::Def::Fn(def))) = cx.defs.get(f.0 as usize) else {
        return vec![];
    };
    let mut block = def.body.block.clone();
    let mut v = AsyncWork { cx, found: vec![] };
    crate::visit::block(&mut block, &mut v);
    v.found
}

struct AsyncWork<'a, 'm> {
    cx: &'a Ctx<'m>,
    found: Vec<(velt_common::Span, String)>,
}

impl crate::visit::VisitMut for AsyncWork<'_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        let what = match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(hir::Intrinsic::Spawn),
                ..
            } => Some("`spawn`".to_string()),
            E::Call {
                callee: Callee::Def(d, _),
                ..
            } => match self.cx.info.get(d.0 as usize) {
                Some(DefInfo::Fn(f)) if self.cx.modules.get(f.module).is_some_and(|m| m.is_std) => {
                    let name = f.name.rsplit("::").next().unwrap_or(&f.name);
                    ["setTimeout", "setInterval", "queueMicrotask"]
                        .contains(&name)
                        .then(|| format!("`{name}`"))
                }
                _ => None,
            },
            _ => None,
        };
        let what = what.or_else(|| {
            matches!(self.cx.ty.kind(e.ty), hir::TyKind::Promise(..))
                .then(|| "a promise".to_string())
        });
        // One per statement: a promise chain's inner promises are the same work.
        if let Some(w) = what {
            let (lo, hi) = (e.span.lo, e.span.hi);
            if !self.found.iter().any(|(s, _)| s.lo <= lo && hi <= s.hi) {
                self.found.retain(|(s, _)| !(lo <= s.lo && s.hi <= hi));
                self.found.push((e.span, w));
            }
        }
    }
}

/// Is global `d` the slot of an imported module's top-level statements?
pub(crate) fn is_script(cx: &Ctx, d: DefId) -> bool {
    cx.module_scripts.contains(&d)
}
