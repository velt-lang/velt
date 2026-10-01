//! Locals, block scopes, and name lookup across closure frames (creating captures).

use velt_syntax::ast;

use super::narrow::Fact;

/// Per open scope: its non-null locals and union member facts (see `FnCx::narrow_state`).
pub(crate) type NarrowState = Vec<(Vec<LocalId>, Vec<(LocalId, Vec<u32>)>)>;
use super::{CaptureCx, FnCx, Frame, LocalKind, Scope};
use crate::ctx::Item;
use crate::hir::{LocalDef, LocalId, TyId};
use velt_common::Span;

fn frame_lookup(f: &Frame, name: &str) -> Option<LocalId> {
    f.scopes
        .iter()
        .rev()
        .find_map(|s| s.names.get(name).copied())
}

/// Narrowing of local `l` in frame `f`: (non-null, union members).
fn frame_narrowing(f: &Frame, l: LocalId) -> (bool, Option<Vec<u32>>) {
    let non_null = f.scopes.iter().any(|s| s.narrowed.contains(&l));
    let members = f.scopes.iter().rev().find_map(|s| {
        s.members
            .iter()
            .rev()
            .find(|(x, _)| *x == l)
            .map(|(_, vs)| vs.clone())
    });
    (non_null, members)
}

/// Capture `outer` into closure frame `f` with `outer`'s narrowing where the closure is created
/// (`narrowing`): a by-value capture copies the narrowed value, a by-reference one is used
/// during the call only, and the closure itself may not assign it (`FnCx::check_capture_assign`).
fn add_narrowed_capture(
    f: &mut Frame,
    name: &str,
    outer: LocalId,
    ty: TyId,
    span: Span,
    narrowing: (bool, Option<Vec<u32>>),
) -> LocalId {
    let inner = add_capture(f, name, outer, ty, span);
    let (non_null, members) = narrowing;
    let root = &mut f.scopes[0];
    if non_null {
        root.narrowed.push(inner);
    }
    let narrowed = non_null || members.is_some();
    if let Some(vs) = members {
        root.members.push((inner, vs));
    }
    if let Some(c) = f.captures.last_mut() {
        c.narrowed = narrowed;
    }
    inner
}

fn add_capture(f: &mut Frame, name: &str, outer: LocalId, ty: TyId, span: Span) -> LocalId {
    let inner = LocalId(f.locals.len() as u32);
    f.locals.push(LocalDef {
        name: name.to_string(),
        ty,
        mutable: false,
        span,
    });
    f.kinds.push(LocalKind::Capture);
    f.captures.push(CaptureCx {
        outer,
        inner,
        mutated: false,
        mutated_at: None,
        narrowed: false,
    });
    f.scopes[0].names.insert(name.to_string(), inner);
    inner
}

impl FnCx<'_, '_> {
    pub fn new_local(
        &mut self,
        name: &str,
        ty: TyId,
        mutable: bool,
        span: Span,
        kind: LocalKind,
    ) -> LocalId {
        let id = LocalId(self.f.locals.len() as u32);
        self.f.locals.push(LocalDef {
            name: name.to_string(),
            ty,
            mutable,
            span,
        });
        self.f.kinds.push(kind);
        id
    }

    /// Declare a named local in the innermost scope (reports same-scope redeclaration).
    pub fn declare_local(&mut self, ident: &ast::Ident, ty: TyId, kind: LocalKind) -> LocalId {
        let mutable = matches!(kind, LocalKind::Let);
        self.declare_local_mut(ident, ty, kind, mutable)
    }

    pub fn declare_local_mut(
        &mut self,
        ident: &ast::Ident,
        ty: TyId,
        kind: LocalKind,
        mutable: bool,
    ) -> LocalId {
        let id = self.new_local(&ident.name, ty, mutable, ident.span, kind);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        if scope.names.contains_key(&ident.name) {
            self.cx.err(
                format!("cannot redeclare block-scoped variable `{}`", ident.name),
                ident.span,
            );
        }
        self.f
            .scopes
            .last_mut()
            .expect("ICE: no scope")
            .names
            .insert(ident.name.clone(), id);
        self.rec_local_decl(id);
        id
    }

    /// A local visible here: the current frame's, or one of an enclosing function (which is then
    /// captured by every closure frame in between).
    pub fn lookup_local(&mut self, name: &str, span: Span) -> Option<LocalId> {
        if let Some(l) = frame_lookup(&self.f, name) {
            return Some(l);
        }
        let j = (0..self.outer.len())
            .rev()
            .find(|&j| frame_lookup(&self.outer[j], name).is_some())?;
        let mut prev = frame_lookup(&self.outer[j], name).expect("ICE: found");
        let mut ty = self.outer[j].locals[prev.0 as usize].ty;
        let mut narrowing = frame_narrowing(&self.outer[j], prev);
        for idx in j + 1..self.outer.len() {
            let f = &mut self.outer[idx];
            prev = add_narrowed_capture(f, name, prev, ty, span, narrowing);
            ty = f.locals[prev.0 as usize].ty;
            narrowing = frame_narrowing(f, prev);
        }
        Some(add_narrowed_capture(
            &mut self.f,
            name,
            prev,
            ty,
            span,
            narrowing,
        ))
    }

    /// Lookup without creating captures (for "is this name a local?" questions).
    pub fn is_local_name(&self, name: &str) -> bool {
        frame_lookup(&self.f, name).is_some()
            || self.outer.iter().any(|f| frame_lookup(f, name).is_some())
    }

    /// A module-level (or visible nested) item named `name`, used as a value at `at` (a name
    /// imported with `import type` is reported).
    pub fn lookup_item(&mut self, name: &str, at: Span) -> Option<Item> {
        let item = self.cx.lookup_item_at(self.module, name, at);
        self.cx.rec_item(at, item);
        if item.is_some() && self.cx.scopes[self.module].type_only.contains(name) {
            self.type_only_value(name, at);
        }
        item
    }

    /// "`T` is imported with `import type`", once per use.
    fn type_only_value(&mut self, name: &str, at: Span) {
        let msg = format!("`{name}` is imported with `import type` and cannot be used as a value");
        let reported = self
            .cx
            .diags
            .iter()
            .any(|d| d.message == msg && d.labels.first().is_some_and(|l| l.span == at));
        if !reported {
            self.cx
                .error(velt_common::Diagnostic::error(msg, at).with_note(format!(
                    "import it as a value: `import {{ {name} }} from …`"
                )));
        }
    }

    /// A scope for narrowing only (it ends where the enclosing one does).
    pub fn push_scope(&mut self) {
        let hi = self.f.scopes.last().map_or(0, |s| s.hi);
        self.push_scope_until(hi);
    }

    /// A block scope ending at source offset `hi`.
    pub fn push_scope_until(&mut self, hi: u32) {
        self.f.scopes.push(Scope {
            hi,
            ..Scope::default()
        });
    }

    pub fn pop_scope(&mut self) {
        if let Some(s) = self.f.scopes.pop() {
            self.rec_scope(&s);
        }
    }

    pub fn is_narrowed(&self, l: LocalId) -> bool {
        self.f.scopes.iter().any(|s| s.narrowed.contains(&l))
    }

    /// Assume `fact` in the innermost scope (member sets intersect with what is known).
    pub fn narrow(&mut self, fact: &Fact) {
        match fact {
            Fact::NonNull(l) => self.innermost_scope().narrowed.push(*l),
            Fact::Members(l, vs) => {
                let vs = match self.allowed_members(*l) {
                    Some(cur) => vs.iter().copied().filter(|v| cur.contains(v)).collect(),
                    None => vs.clone(),
                };
                self.innermost_scope().members.push((*l, vs));
            }
        }
    }

    fn innermost_scope(&mut self) -> &mut super::Scope {
        self.f.scopes.last_mut().expect("ICE: no scope")
    }

    /// The variants a union local is narrowed to here (`None`: not narrowed).
    pub fn allowed_members(&self, l: LocalId) -> Option<Vec<u32>> {
        self.f.scopes.iter().rev().find_map(|s| {
            s.members
                .iter()
                .rev()
                .find(|(x, _)| *x == l)
                .map(|(_, vs)| vs.clone())
        })
    }

    /// Is some union local narrowed to none of its members here (so this code cannot run)?
    pub fn exhausted_union_local(&self) -> bool {
        let locals: Vec<LocalId> = self
            .f
            .scopes
            .iter()
            .flat_map(|s| s.members.iter().map(|(l, _)| *l))
            .collect();
        locals.into_iter().any(|l| {
            let nullable = self.cx.ty.opt_payload(self.local_ty(l)).is_some();
            self.allowed_members(l).is_some_and(|vs| vs.is_empty())
                && (!nullable || self.is_narrowed(l))
        })
    }

    /// The narrowing facts of every open scope, so sibling branches (`if`/`else`, ternary and
    /// `match` arms) are each checked from the state before them: a reassignment in one branch
    /// does not affect its siblings.
    pub fn narrow_state(&self) -> NarrowState {
        self.f
            .scopes
            .iter()
            .map(|s| (s.narrowed.clone(), s.members.clone()))
            .collect()
    }

    pub fn restore_narrowing(&mut self, st: &NarrowState) {
        for (s, (n, m)) in self.f.scopes.iter_mut().zip(st) {
            s.narrowed = n.clone();
            s.members = m.clone();
        }
    }

    /// After sibling branches that both continue: keep only the facts `other` (the state at the
    /// end of the other branch) still has.
    pub fn meet_narrowing(&mut self, other: &NarrowState) {
        for (s, (n, m)) in self.f.scopes.iter_mut().zip(other) {
            s.narrowed.retain(|l| n.contains(l));
            s.members.retain(|f| m.contains(f));
        }
    }

    /// The local was reassigned: its narrowing no longer holds.
    pub fn unnarrow(&mut self, l: LocalId) {
        let fields = self.field_tokens_of(l);
        for s in &mut self.f.scopes {
            s.narrowed.retain(|x| *x != l && !fields.contains(x));
            s.members.retain(|(x, _)| *x != l);
        }
    }

    /// Assigning local `l` at `span`: an error when `l` is a capture that starts narrowed (the
    /// closure may run again with the new value, which the narrowing would not allow).
    pub fn check_capture_assign(&mut self, l: LocalId, span: Span) {
        if !self.f.captures.iter().any(|c| c.inner == l && c.narrowed) {
            return;
        }
        let name = self.f.locals[l.0 as usize].name.clone();
        self.cx.error(
            velt_common::Diagnostic::error(
                format!("cannot assign to `{name}` here: it is narrowed where the closure is created"),
                span,
            )
            .with_note(format!(
                "the closure relies on the check made outside it; copy `{name}` into a local of the closure first"
            )),
        );
    }

    /// Record that a captured variable is mutated inside the closure.
    pub fn mark_mutated(&mut self, l: LocalId, span: Span) {
        if let Some(c) = self.f.captures.iter_mut().find(|c| c.inner == l) {
            c.mutated = true;
            c.mutated_at.get_or_insert(span);
        }
        self.f.locals[l.0 as usize].mutable = true;
    }
}
