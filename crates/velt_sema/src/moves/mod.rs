//! Flow-sensitive ownership check over a function's HIR: use after move and use before init.
//!
//! State per program point: see [`state`]. Branches join with OR, so a value moved in one
//! branch is an error to use after the branch. Loops iterate to a fixpoint silently, then run
//! one reporting pass ([`loops`]), so a move inside a loop body is reported at its
//! second-iteration use. A use blames every move that reaches it (a soft move in a loop body
//! and one before the loop, one per branch). Assignment re-initializes. Moves are tracked per
//! place path (`p.first` and `p.second` can be moved separately; [`expr`]); any use of a place
//! overlapping a moved path is an error. Escaping closures move the variables they capture by
//! value. A `using` local is never moved: it is disposed at the end of its block.

mod expr;
mod loops;
mod state;
mod tries;

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
    /// Non-escaping closures held in a local, by local (`crate::ownership::held_closures`):
    /// each call through the local uses what the closure borrows.
    held: HashMap<LocalId, DefId>,
    /// The local a `let` being checked binds (the holder of a closure literal initializer).
    holder: Option<LocalId>,
    /// The `let` locals of each block being checked (innermost last).
    open: Vec<Vec<usize>>,
    /// Inside a `for` loop's step (`i++`): a fresh binding per iteration in JS, so a closure's
    /// copy of the loop variable is what JS sees too.
    in_step: bool,
    never: TyId,
    /// Per local of the function: is its value shared (`Ctx::is_shared_value`) rather than
    /// moved when an escaping closure captures it and it is used again?
    shared: Vec<bool>,
    report: bool,
    errors: Vec<Diagnostic>,
    loops: Vec<LoopFlow>,
    /// Soft moves of this function (`FnInfo::soft_moves`) and those whose place is used again
    /// (with the local used again: a closure's soft move is one per captured variable).
    soft: HashSet<Span>,
    reused: HashSet<(Span, LocalId)>,
    /// Per escaping closure: the enclosing variables it captures by value and assigns.
    writers: &'a HashMap<DefId, HashSet<LocalId>>,
    /// Closures each call of which has a state of its own that shares the captured variables:
    /// generator closures (`function*` expressions) and local async closures
    /// (`FnDef::shares_captures`); true for generators.
    generators: &'a HashMap<DefId, bool>,
    /// Per local: may it live in a shared cell (not a promise, no async closure captures it)?
    boxable: Vec<bool>,
    /// Locals that need a shared cell (`LocalDef::boxed`, see `crate::ownership::cells`).
    boxed: HashSet<LocalId>,
    /// The open `try` bodies and handlers (innermost last), see [`tries`].
    tries: Vec<tries::TryFrame>,
}

/// What the move dataflow found besides errors.
#[derive(Default)]
pub(crate) struct Outcome {
    /// Per function: the soft moves whose place is used again, with the local used again (they
    /// become shares, `crate::ownership::soft`; for a closure, only the captures of that local).
    pub reused: HashMap<DefId, HashSet<(Span, LocalId)>>,
    /// Per function: the variables that need a shared cell (`crate::ownership::cells`).
    pub boxed: HashMap<DefId, HashSet<LocalId>>,
}

/// Check every function body.
pub(crate) fn check_all(cx: &mut Ctx) -> Outcome {
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
    let never = cx.ty.never;
    let local_tys: Vec<Vec<TyId>> = cx
        .defs
        .iter()
        .map(|d| match d {
            Some(Def::Fn(f)) => f.body.locals.iter().map(|l| l.ty).collect(),
            _ => vec![],
        })
        .collect();
    let copy: Vec<Vec<bool>> = local_tys
        .iter()
        .map(|tys| tys.iter().map(|&t| cx.is_copy(t)).collect())
        .collect();
    let shared: Vec<Vec<bool>> = local_tys
        .into_iter()
        .map(|tys| tys.into_iter().map(|t| cx.is_shared_value(t)).collect())
        .collect();
    let writers = writers(cx, &escaping);
    let async_captured = async_captured(cx);
    let generators: HashMap<DefId, bool> = escaping
        .iter()
        .filter_map(|&d| match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) if f.is_generator || f.shares_captures => Some((d, f.is_generator)),
            _ => None,
        })
        .collect();
    let mut all = vec![];
    let mut out = Outcome::default();
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
            held: crate::ownership::held_closures(cx, &f.body.block),
            in_step: false,
            holder: None,
            open: vec![],
            never,
            boxable: shared[i]
                .iter()
                .zip(&copy[i])
                .enumerate()
                .map(|(l, (s, c))| {
                    (*s || *c) && !async_captured.contains(&(def, LocalId(l as u32)))
                })
                .collect(),
            shared: shared[i].clone(),
            writers: &writers,
            generators: &generators,
            boxed: HashSet::new(),
            tries: vec![],
            report: false,
            errors: vec![],
            loops: vec![],
            soft,
            reused: HashSet::new(),
        };
        // A silent pass first (only bodies creating an escaping closure can need a cell): a
        // variable found to need a shared cell late in the body is no move error earlier either.
        if creates_escaping(&f.body.block, &escaping) {
            m.report = false;
            m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
        }
        m.report = true;
        m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
        all.extend(
            m.errors
                .into_iter()
                .filter(|d| !follows_capture_error(cx, d)),
        );
        if !m.reused.is_empty() {
            out.reused.insert(def, m.reused);
        }
        if !m.boxed.is_empty() {
            out.boxed.insert(def, m.boxed);
        }
    }
    let mut seen = HashSet::new();
    for d in all {
        let key = (d.labels[0].span.lo, d.labels[0].span.hi, d.message.clone());
        if seen.insert(key) {
            cx.error(d);
        }
    }
    out
}

/// Per escaping closure: the enclosing variables it captures by value and assigns.
fn writers(cx: &mut Ctx, escaping: &HashSet<DefId>) -> HashMap<DefId, HashSet<LocalId>> {
    let mut out = HashMap::new();
    for &c in escaping {
        let Some(Def::Fn(mut f)) = cx.defs[c.0 as usize].take() else {
            continue;
        };
        let mut assigned = HashSet::new();
        crate::visit::exprs_mut(&mut f.body.block, &mut |e: &mut crate::hir::Expr| {
            use crate::hir::ExprKind as E;
            if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                if let E::Local(l, _) = place.kind {
                    assigned.insert(l);
                }
            }
        });
        let outer: HashSet<LocalId> = f
            .captures
            .iter()
            .filter(|cap| assigned.contains(&cap.inner))
            .map(|cap| cap.outer)
            .collect();
        cx.defs[c.0 as usize] = Some(Def::Fn(f));
        out.insert(c, outer);
    }
    out
}

/// `(function, local)` pairs captured by an async closure that may run on another thread: such
/// a variable keeps its own value per closure (cells are not atomic). A local async closure
/// (`FnDef::shares_captures`, `crate::ownership::local_async`) and a generator closure's
/// generators stay on the task that creates them, so their captures may live in cells.
fn async_captured(cx: &mut Ctx) -> HashSet<(DefId, LocalId)> {
    let mut out = HashSet::new();
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        let mut closures = vec![];
        crate::visit::exprs_mut(&mut f.body.block, &mut |e: &mut crate::hir::Expr| {
            if let crate::hir::ExprKind::Closure(c) = e.kind {
                closures.push(c);
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
        for c in closures {
            if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
                if cf.is_async && !cf.is_generator && !cf.shares_captures {
                    let d = DefId(i as u32);
                    out.extend(cf.captures.iter().map(|cap| (d, cap.outer)));
                }
            }
        }
    }
    out
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
        if self.watch_cell(s, i, closure) {
            s.clear(i);
        }
        if s.overlaps(i, path) {
            // Every move that may have happened is blamed: soft ones become shares; any other
            // makes this use an error.
            let sites = &s.moved_at[i];
            let hard = sites.iter().find(|(_, kind)| *kind != MoveKind::Soft);
            if self.report {
                match hard {
                    None if !sites.is_empty() => {
                        self.reused.extend(sites.iter().map(|(at, _)| (*at, l)));
                    }
                    _ => {
                        let d = self.moved_error(i, hard.copied(), span);
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
        let Some((at, holder, writes)) = s.captured[i].take() else {
            return;
        };
        if self.in_step {
            return;
        }
        if self.boxable[i] {
            // The closure and this function see one variable: it lives in a shared cell.
            self.boxed.insert(l);
            s.captured[i] = Some((at, holder, writes));
            return;
        }
        if !self.report {
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

    /// A use of local `i` (by this function, or a capture by a closure: `closure`) while an
    /// escaping closure that assigns it holds it: the variable needs a shared cell. Returns
    /// whether it is (now) boxed, so a move into that closure is no move.
    fn watch_cell(&mut self, s: &mut State, i: usize, closure: bool) -> bool {
        let l = LocalId(i as u32);
        if s.captured[i].is_some_and(|(_, _, writes)| writes) && self.boxable[i] && !closure {
            self.boxed.insert(l);
        }
        self.boxed.contains(&l)
    }

    /// A generator closure (`generator`) or a local async closure created here assigns
    /// enclosing variable `l` (module docs of `crate::ownership::cells`): it lives in a cell,
    /// or, when it cannot, that is an error.
    fn generator_writes(&mut self, l: LocalId, span: Span, generator: bool) {
        let i = l.0 as usize;
        if self.boxable[i] {
            self.boxed.insert(l);
            return;
        }
        if !self.report {
            return;
        }
        let name = &self.locals[i].name;
        let (what, calls) = match generator {
            true => ("a generator function expression", "the generators"),
            false => ("an async closure", "the closure's calls"),
        };
        self.errors.push(
            Diagnostic::error(format!("{what} cannot change `{name}` here"), span).with_note(
                format!(
                    "TypeScript allows this; Velt doesn't because `{name}` is also captured by an async closure that may run on another thread (or holds a promise), so it cannot be shared with {calls}; share the value with `shared`: `const {name} = shared(...)` and `{name}.add(n)` / `{name}.set(v)`, or pass it in as a parameter"
                ),
            ),
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
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
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
                self.leave_return(st);
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
}

/// Does block `b` create an escaping closure?
fn creates_escaping(b: &crate::hir::Block, escaping: &HashSet<DefId>) -> bool {
    let mut b = b.clone();
    let mut found = false;
    crate::visit::exprs_mut(&mut b, &mut |e: &mut crate::hir::Expr| {
        if let crate::hir::ExprKind::Closure(c) = e.kind {
            found |= escaping.contains(&c);
        }
    });
    found
}

/// A "use of moved value `x`" whose move is into a closure already reported for modifying the
/// captured `x` (`ownership::local_async`): the first error says it all.
fn follows_capture_error(cx: &Ctx, d: &Diagnostic) -> bool {
    let Some(name) = d
        .message
        .strip_prefix("use of moved value `")
        .and_then(|r| r.strip_suffix('`'))
    else {
        return false;
    };
    d.labels.iter().skip(1).any(|l| {
        cx.reported_captures.iter().any(|(n, span)| {
            n == name && l.span.file == span.file && span.lo <= l.span.lo && l.span.lo < span.hi
        })
    })
}
