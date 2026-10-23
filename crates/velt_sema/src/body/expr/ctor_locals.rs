//! Class values kept in a local (`ctor_values`): where a class's value can't be a module
//! constant, it is made by a `const` at the start of a block and every read of the class as a
//! value of that type in the block reads it, so two reads are one object, as in JavaScript.
//!
//! - A class declared in a function (`collect::nested`): the block that declares it, whose
//!   start is where JavaScript makes the class object (a class can't be named before its
//!   declaration), so each run of the block (each call, each loop iteration) makes a new one;
//! - a module-level class read as a construct signature over type parameters (in a generic
//!   function, `const k: new (v: T) => Box<T> = Box`): the function's body, so it is one object
//!   per call (JavaScript has one per class; values made by two calls differ here).
//!
//! The pending `const`s are kept per frame ([`ClassLet`]) and put in place when the statements
//! of their block are done ([`FnCx::close_class_lets`]); reads from a closure in the block
//! capture the local by its hidden name, as any variable.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::ctx::Item;
use crate::hir::{self, DefId, LocalDef, LocalId, TyId};

/// A class value pending as a `const` at the start of the block of scope `scope`.
#[derive(Clone)]
pub(crate) struct ClassLet {
    scope: usize,
    key: (DefId, TyId),
    /// The local's type.
    ty: TyId,
    name: String,
    stmt: hir::Stmt,
}

impl FnCx<'_, '_> {
    /// The frame (`None`: the current one, `Some(j)`: `outer[j]`) and scope of the open block
    /// whose start holds class `d`'s value of type `target` (module docs).
    fn class_let_block(&self, d: DefId, target: TyId) -> Option<(Option<usize>, usize)> {
        let frames = || {
            let outer = self.outer.iter().enumerate().map(|(j, f)| (Some(j), f));
            outer.chain(std::iter::once((None, &self.f)))
        };
        let nested = self
            .cx
            .nested
            .iter()
            .find(|n| matches!(n.item, Item::Def(x) if x == d));
        if let Some(n) = nested {
            if n.module != self.module {
                return None;
            }
            let found = frames()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .find_map(|(j, f)| {
                    let s = f.stmt_lists.iter().find(|&&s| f.scopes[s].hi == n.hi)?;
                    Some((j, *s))
                });
            return found;
        }
        if !self.cx.has_type_params(target) {
            return None;
        }
        let (j, f) = frames().next()?;
        Some((j, *f.stmt_lists.first()?))
    }

    fn frame_at(&mut self, j: Option<usize>) -> &mut Frame {
        match j {
            Some(j) => &mut self.outer[j],
            None => &mut self.f,
        }
    }

    /// Class `d`'s value of type `target` read from its block's `const` (module docs), made
    /// on first use from literal `lit`; `None` when the class has no such block here.
    pub(super) fn class_value_local(
        &mut self,
        d: DefId,
        (target, key): (TyId, TyId),
        lit: impl FnOnce(&mut Self) -> Option<ast::Expr>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.detached {
            return None;
        }
        let (j, scope) = self.class_let_block(d, target)?;
        let key = (d, key);
        let lets = &self.frame_at(j).class_lets;
        let made: Vec<((DefId, TyId), TyId, String)> = lets
            .iter()
            .filter(|c| c.key.0 == d)
            .map(|c| (c.key, c.ty, c.name.clone()))
            .collect();
        // Signature types written apart over type parameters are one but for their variables.
        let known = made.into_iter().find(|(k, ty, _)| {
            *k == key || (self.cx.fn_sites.use_vars && self.cx.unify_ctor_vars(*ty, target))
        });
        let name = match known {
            Some((_, _, name)) => name,
            None => {
                let lit = lit(self)?;
                let before = self.f.locals.len();
                let init = self.expr(&lit, Some(target), Want::Move);
                // A temporary of this frame can't be read from the block's start in another.
                if j.is_some() && self.f.locals.len() != before {
                    return Some(init);
                }
                let f = self.frame_at(j);
                let name = format!("#class{}#{}", d.0, f.class_lets.len());
                let local = LocalId(f.locals.len() as u32);
                f.locals.push(LocalDef {
                    name: name.clone(),
                    ty: target,
                    mutable: false,
                    boxed: false,
                    span,
                });
                f.kinds.push(LocalKind::Const);
                f.scopes[scope].names.insert(name.clone(), local);
                let kind = hir::StmtKind::Let {
                    local,
                    init: Some(init),
                };
                f.class_lets.push(ClassLet {
                    scope,
                    key,
                    ty: target,
                    name: name.clone(),
                    stmt: hir::Stmt { kind, span },
                });
                name
            }
        };
        let id = ast::Ident { name, span };
        let read = self.ident_expr(&id, Some(target), want);
        Some(self.coerce(read, target))
    }

    /// A list of statements of the innermost scope starts (`stmts_into`): its class values'
    /// `const`s go before its first statement, at `out.len()`.
    pub(crate) fn open_class_lets(&mut self, out: &[hir::Stmt]) -> usize {
        let s = self.f.scopes.len() - 1;
        self.f.stmt_lists.push(s);
        out.len()
    }

    /// The list opened by `open_class_lets` (at `at` in `out`) is done: the class values made
    /// for its scope go in place, unless an enclosing list of the same scope (the rest of a
    /// block after a `using` declaration) is still open, which takes them.
    pub(crate) fn close_class_lets(&mut self, at: usize, out: &mut Vec<hir::Stmt>) {
        let Some(s) = self.f.stmt_lists.pop() else {
            return;
        };
        if self.f.stmt_lists.contains(&s) || self.f.class_lets.is_empty() {
            return;
        }
        let (mine, rest) = std::mem::take(&mut self.f.class_lets)
            .into_iter()
            .partition::<Vec<_>, _>(|c| c.scope >= s);
        self.f.class_lets = rest;
        let at = at.min(out.len());
        out.splice(at..at, mine.into_iter().map(|c| c.stmt));
    }
}
