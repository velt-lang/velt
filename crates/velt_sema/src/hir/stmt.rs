//! HIR statements and blocks (moved out of `hir/mod.rs`, unchanged).

use velt_common::Span;

use super::*;

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Trailing value expression (used for block-valued `match` arms etc.). Usually `None`.
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `let`/`const` with a simple binding. `init` None ⇒ uninitialized until assigned.
    Let {
        local: LocalId,
        init: Option<Expr>,
    },
    /// M2+ draft: destructuring `let { a, b } = e` / `let [x, y] = e`.
    LetPat {
        pat: Pat,
        init: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Block,
        els: Option<Block>,
    },
    /// `step` runs after the body and on `continue` (from desugared C-style `for`).
    While {
        label: Option<String>,
        cond: Expr,
        body: Block,
        step: Option<Expr>,
    },
    /// `for (const x of iter)` over arrays (elements borrowed, or copied if Copy).
    ForOf {
        label: Option<String>,
        binding: Pat,
        iter: Expr,
        body: Block,
        /// The loop consumes `iter` (an owned temporary array, see hir_encodings.md): each
        /// element is moved into `binding` (owned bindings, `UseMode::Move`); elements not yet
        /// visited when the loop is left early are dropped, then the buffer is freed.
        consume: bool,
        /// A borrowing loop whose body may replace or remove (a part of) the element a binding
        /// refers to (`xs[i] = …` on the iterated array): each borrowing binding takes a share
        /// of what it binds, released with the iteration, as JS keeps the old element alive
        /// (hir_encodings.md). Set by the exclusive-access check; `false` everywhere else.
        share_elems: bool,
    },
    /// `try { body } catch (e) { handler } finally { fin }`: the catch local has the union `body`
    /// can throw (`None` / `Never`: nothing is caught; see hir_encodings.md "Errors").
    Try {
        body: Block,
        catch: Option<(Option<LocalId>, Block)>,
        finally: Option<Block>,
    },
    Break(Option<String>),
    Continue(Option<String>),
    Block(Block),
}
