//! Flow-sensitive ownership check over a function's HIR: use after move and use before init.
//!
//! State per program point: see [`state`]. Branches join with OR, so a value moved in one
//! branch is an error to use after the branch. Loops iterate to a fixpoint silently, then run
//! one reporting pass ([`loops`]), so a move inside a loop body is reported at its
//! second-iteration use. Assignment re-initializes. Moves are tracked per place path
//! (`p.first` and `p.second` can be moved separately; [`expr`]); any use of a place overlapping
//! a moved path is an error. Escaping closures move the variables they capture by value.
//! A `using` local is never moved: it is disposed at the end of its block.

mod expr;
mod loops;
mod state;

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::hir::{
    Block, Capture, Def, DefId, LocalDef, LocalId, Pat, PatKind, Stmt, StmtKind, TyId, UseMode,
};
use loops::LoopFlow;
use state::{join, Flow, MoveKind, State};

struct Moves<'a> {
    locals: &'a [LocalDef],
    /// Per local: declared with `using` / `await using`.
    using: Vec<bool>,
    captures: &'a HashMap<DefId, Vec<Capture>>,
    /// Closures that are stored or returned (they capture by value).
    escaping: &'a HashSet<DefId>,
    /// The local a `let` being checked binds (the holder of a closure literal initializer).
    holder: Option<LocalId>,
    /// The `let` locals of each block being checked (innermost last).
    open: Vec<Vec<usize>>,
    /// Inside a `for` loop's step (`i++`): a fresh binding per iteration in JS, so a closure's
    /// copy of the loop variable is what JS sees too.
    in_step: bool,
    never: TyId,
    str_: TyId,
    report: bool,
    errors: Vec<Diagnostic>,
    loops: Vec<LoopFlow>,
    /// Soft moves of this function (`FnInfo::soft_moves`) and those whose place is used again.
    soft: HashSet<Span>,
    reused: HashSet<Span>,
}

/// Check every function body. Returns, per function, the soft moves (async-call arguments,
/// strings) whose place is used again: they must become clones (`crate::ownership::soft`).
pub(crate) fn check_all(cx: &mut Ctx) -> HashMap<DefId, HashSet<Span>> {
    let mut captures = HashMap::new();
    let mut escaping = HashSet::new();
    for (i, d) in cx.defs.iter().enumerate() {
        if let Some(Def::Fn(f)) = d {
            let def = DefId(i as u32);
            captures.insert(def, f.captures.clone());
            if cx.try_fn(def).is_some_and(|info| info.escaping) {
                escaping.insert(def);
            }
        }
    }
    let (never, str_) = (cx.ty.never, cx.ty.str_);
    let mut all = vec![];
    let mut reused = HashMap::new();
    for (i, d) in cx.defs.iter().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let def = DefId(i as u32);
        let soft = cx
            .try_fn(def)
            .map(|info| info.soft_moves.iter().copied().collect())
            .unwrap_or_default();
        let using = cx
            .try_fn(def)
            .map(|info| {
                let kinds = &info.local_kinds;
                kinds.iter().map(|k| *k == LocalKind::Using).collect()
            })
            .unwrap_or_default();
        let mut m = Moves {
            locals: &f.body.locals,
            using,
            captures: &captures,
            escaping: &escaping,
            in_step: false,
            holder: None,
            open: vec![],
            never,
            str_,
            report: true,
            errors: vec![],
            loops: vec![],
            soft,
            reused: HashSet::new(),
        };
        let mut st = Some(State::new(f.body.locals.len()));
        m.block(&f.body.block, &mut st);
        all.extend(m.errors);
        if !m.reused.is_empty() {
            reused.insert(def, m.reused);
        }
    }
    let mut seen = HashSet::new();
    for d in all {
        let key = (d.labels[0].span.lo, d.labels[0].span.hi, d.message.clone());
        if seen.insert(key) {
            cx.error(d);
        }
    }
    reused
}

impl Moves<'_> {
    /// A use of `path` of local `l`; `closure`: the move is an escaping closure's capture.
    fn use_path(
        &mut self,
        l: LocalId,
        path: &[u32],
        mode: UseMode,
        span: Span,
        st: &mut Flow,
        closure: bool,
    ) {
        let Some(s) = st else { return };
        let i = l.0 as usize;
        if s.overlaps(i, path) {
            match s.moved_at[i] {
                Some((at, MoveKind::Soft)) => {
                    if self.report {
                        self.reused.insert(at);
                    }
                }
                moved_at => {
                    if self.report {
                        let d = self.moved_error(i, moved_at, span);
                        self.errors.push(d);
                    }
                }
            }
            s.clear(i);
        } else if s.uninit[i] {
            if self.report {
                let name = &self.locals[i].name;
                self.errors.push(Diagnostic::error(
                    format!("use of possibly uninitialized variable `{name}`"),
                    span,
                ));
            }
            s.uninit[i] = false;
        }
        if mode == UseMode::Move {
            self.check_using_move(i, span);
            if path.is_empty() {
                s.escape_holder(i);
            }
            let kind = if closure {
                MoveKind::Closure
            } else if self.soft.contains(&span) {
                MoveKind::Soft
            } else {
                MoveKind::Plain
            };
            s.mark_moved(i, path, span, kind);
        }
    }

    /// Local `l` is assigned (or modified by a closure): an error once an escaping closure has
    /// captured it by value, since the closure would keep seeing the old value (JS closures see
    /// the variable itself).
    fn assigned(&mut self, l: LocalId, span: Span, st: &mut Flow) {
        let Some(s) = st else { return };
        let i = l.0 as usize;
        let Some((at, _)) = s.captured[i].take() else {
            return;
        };
        if !self.report || self.in_step {
            return;
        }
        let name = &self.locals[i].name;
        self.errors.push(
            Diagnostic::error(
                format!("cannot assign to `{name}` after a stored closure captured it"),
                span,
            )
            .with_label(at, "captured here")
            .with_note(format!(
                "the closure keeps its own copy of `{name}` and would not see the new value; assign `{name}` before creating the closure, use a separate variable, or share it with `shared(...)`"
            )),
        );
    }

    fn moved_error(&self, i: usize, moved_at: Option<(Span, MoveKind)>, span: Span) -> Diagnostic {
        let name = &self.locals[i].name;
        let d = Diagnostic::error(format!("use of moved value `{name}`"), span);
        match moved_at {
            Some((at, MoveKind::Closure)) => d.with_label(at, "value moved into closure here").with_note(format!(
                "`{name}` was moved into a closure that is stored or returned; use `shared({name})` to share it, or `{name}.clone()` to give the closure its own copy"
            )),
            Some((at, _)) => d.with_label(at, "value moved here").with_note(format!(
                "`{name}` has a type that is not Copy; use it before moving it, pass it to a function instead (a borrow), or move a copy: `{name}.clone()`"
            )),
            None => d,
        }
    }

    /// Moving out of a `using` local is an error, except the `await using` cleanup call, which
    /// carries the span of the declared name (`crate::body::using`), and soft moves (an async
    /// call's receiver or argument: a clone when the place is used again, as the cleanup does).
    fn check_using_move(&mut self, i: usize, span: Span) {
        let local = &self.locals[i];
        let using = self.using.get(i).copied().unwrap_or(false);
        if !self.report || !using || span == local.span || self.soft.contains(&span) {
            return;
        }
        let name = &local.name;
        self.errors.push(
            Diagnostic::error(format!("cannot move `{name}` out of its `using` declaration"), span)
                .with_label(local.span, "declared with `using` here")
                .with_note(format!(
                    "`{name}` is disposed at the end of its block; pass it to a function that borrows it, or declare it with `const` to hand it over"
                )),
        );
    }

    fn init_local(l: LocalId, st: &mut Flow) {
        if let Some(s) = st {
            s.reinit(l.0 as usize, &[]);
        }
    }

    fn init_pat(p: &Pat, st: &mut Flow) {
        match &p.kind {
            PatKind::Binding(l, _) => Self::init_local(*l, st),
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|x| Self::init_pat(x, st));
                if let Some(l) = rest {
                    Self::init_local(*l, st);
                }
            }
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|x| Self::init_pat(x, st))
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| Self::init_pat(x, st)),
            PatKind::Some(x) => Self::init_pat(x, st),
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => {}
        }
    }

    fn block(&mut self, b: &Block, st: &mut Flow) {
        let lets: Vec<usize> = b
            .stmts
            .iter()
            .filter_map(|s| match s.kind {
                StmtKind::Let { local, .. } => Some(local.0 as usize),
                _ => None,
            })
            .collect();
        self.open.push(lets);
        for s in &b.stmts {
            self.stmt(s, st);
        }
        if let Some(v) = &b.value {
            self.expr(v, st);
        }
        let lets = self.open.pop().unwrap_or_default();
        if let Some(s) = st {
            lets.iter().for_each(|l| s.drop_holder(*l));
        }
    }

    fn stmt(&mut self, s: &Stmt, st: &mut Flow) {
        match &s.kind {
            StmtKind::Let { local, init } => self.let_stmt(*local, init.as_ref(), st),
            StmtKind::LetPat { pat, init } => {
                self.expr(init, st);
                Self::init_pat(pat, st);
            }
            StmtKind::Expr(e) => self.expr(e, st),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, st);
                }
                *st = None;
            }
            StmtKind::If { cond, then, els } => {
                self.expr(cond, st);
                let mut other = st.clone();
                self.block(then, st);
                if let Some(b) = els {
                    self.block(b, &mut other);
                }
                *st = join(st.take(), other);
            }
            StmtKind::While { .. } | StmtKind::ForOf { .. } => self.loop_stmt(s, st),
            StmtKind::Try {
                body,
                catch,
                finally,
            } => self.try_stmt(body, catch.as_ref(), finally.as_ref(), st),
            StmtKind::Break(label) => self.jump(label.as_deref(), true, st),
            StmtKind::Continue(label) => self.jump(label.as_deref(), false, st),
            StmtKind::Block(b) => self.block(b, st),
        }
    }

    fn let_stmt(&mut self, local: LocalId, init: Option<&crate::hir::Expr>, st: &mut Flow) {
        match init {
            Some(e) => {
                self.holder = Some(local);
                self.expr(e, st);
                self.holder = None;
                Self::init_local(local, st);
            }
            None => {
                if let Some(s) = st {
                    let i = local.0 as usize;
                    s.clear(i);
                    s.uninit[i] = true;
                }
            }
        }
    }

    /// A throw can leave the `try` body anywhere: the handler starts from the join of the
    /// states at entry and at the end of the body.
    fn try_stmt(
        &mut self,
        body: &Block,
        catch: Option<&(Option<LocalId>, Block)>,
        finally: Option<&Block>,
        st: &mut Flow,
    ) {
        let entry = st.clone();
        self.block(body, st);
        if let Some((local, handler)) = catch {
            let mut h = join(entry, st.clone());
            if let Some(l) = local {
                Self::init_local(*l, &mut h);
            }
            self.block(handler, &mut h);
            *st = join(st.take(), h);
        }
        if let Some(f) = finally {
            self.block(f, st);
        }
    }
}
