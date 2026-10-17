//! Top-level statements of imported modules (#1201). The parser turns a module's top-level
//! statements into a generated `main` whose name has an empty span (`velt_syntax`
//! `parser::script`). In the root file that is the program's `main`; in an imported module it
//! runs once when the module is evaluated, as in Node: it becomes a slot of the module state
//! (`module_state`), of type `void`, whose initializer calls it. The slot is ordered with the
//! module's other slots by where the statements start, and the modules in Node's evaluation
//! order, so the statements run after the modules the module imports and before the modules
//! that import it. A module that is only imported for its types (`import type`) is never
//! evaluated: its statements don't run (`module_state::order`).

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::{self, Callee, DefId, ExprKind as E};
use crate::SourceModule;

/// The generated `main` of module `m`'s top-level statements, if it has any.
pub(crate) fn script_fn(m: &SourceModule) -> Option<&ast::FnDecl> {
    m.ast.items.iter().find_map(|it| match &it.kind {
        ast::ItemKind::Function(f)
            if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
        {
            Some(f)
        }
        _ => None,
    })
}

/// Give each imported module's top-level statements a module state slot (module docs); after
/// the bodies are checked, so the generated function's result type is known.
pub(crate) fn declare(cx: &mut Ctx) {
    let (modules, root) = (cx.modules, cx.root);
    for (m, module) in modules.iter().enumerate() {
        let Some(script) = script_fn(module).filter(|_| m != root) else {
            continue;
        };
        let span = script.sig.span;
        let Some(Item::Def(f)) = cx.scopes[m].items.get("main").copied() else {
            continue;
        };
        let Some(DefInfo::Fn(info)) = cx.info.get(f.0 as usize) else {
            continue;
        };
        if info.is_async {
            cx.error(
                Diagnostic::error(
                    "top-level `await` in an imported module is not supported yet",
                    span,
                )
                .with_note("only the file the program starts from may `await` at the top level; put the code in an `async` function and call it from there"),
            );
            continue;
        }
        let ret = info.ret;
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

/// Is global `d` the slot of an imported module's top-level statements?
pub(crate) fn is_script(cx: &Ctx, d: DefId) -> bool {
    cx.module_scripts.contains(&d)
}
