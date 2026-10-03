//! The side tables `check_for_ide` fills while checking (`Ctx::ide`; `None` for `check`, so
//! compiling pays one branch per hook): which definition each name use and declaration denotes,
//! the type of each checked expression and local, and where each local is visible.

use velt_common::{FileId, Span};

use crate::body::{FnCx, LocalKind, Scope};
use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, ExprKind as H, LocalId, TyId, TyKind};

/// What a recorded name denotes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    /// A function, method, getter, constructor, type, constant or static field.
    Def(DefId),
    /// Field `index` of struct/class `DefId` (all fields, base classes first) or of an
    /// interface (own then inherited).
    Field(DefId, u32),
    /// Variant `index` of an enum.
    Variant(DefId, u32),
    /// Method slot of an interface.
    IfaceMethod(DefId, u32),
    /// A type alias (`Ctx::aliases` index).
    Alias(u32),
    Local(LocalTarget),
}

impl Target {
    pub fn of_item(item: Item) -> Target {
        match item {
            Item::Def(d) => Target::Def(d),
            Item::Alias(a) => Target::Alias(a),
        }
    }
}

/// A local variable, parameter or `this`, identified by its declaring identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LocalTarget {
    pub decl: Span,
    pub name: String,
    pub ty: TyId,
    /// Display context (generic parameter names) of `ty`.
    pub ctx: u32,
    pub is_param: bool,
    pub mutable: bool,
}

/// A local visible in `[lo, hi)` of `file`.
pub(crate) struct ScopeEntry {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
    pub name: String,
    pub target: Target,
}

#[derive(Default)]
pub(crate) struct Recorder {
    pub refs: Vec<(Span, Target)>,
    /// Checked expressions and declared locals: span, type, display context.
    pub types: Vec<(Span, TyId, u32)>,
    pub scopes: Vec<ScopeEntry>,
    /// Generic parameter names of each display context.
    pub params: Vec<Vec<String>>,
    /// `const f = (x) => ...`: declaring identifier → closure, so `f` displays with the
    /// closure's parameter names.
    pub closures: std::collections::HashMap<Span, DefId>,
}

impl Ctx<'_> {
    pub fn recording(&self) -> bool {
        self.ide.is_some()
    }

    /// `span` names `t`.
    pub fn rec_ref(&mut self, span: Span, t: Target) {
        if let Some(r) = &mut self.ide {
            r.refs.push((span, t));
        }
    }

    /// How many name references are recorded so far (a mark for [`Ctx::rec_mirror`]).
    pub fn rec_mark(&self) -> usize {
        self.ide.as_ref().map_or(0, |r| r.refs.len())
    }

    /// Whatever was recorded at `from` since `mark` is named at `to` too (a closing tag's name
    /// denotes what its opening tag's does).
    pub fn rec_mirror(&mut self, mark: usize, from: Span, to: Span) {
        if let Some(r) = &mut self.ide {
            let recent = r.refs.get(mark..).unwrap_or_default();
            if let Some((_, t)) = recent.iter().rev().find(|(s, _)| *s == from) {
                let t = t.clone();
                r.refs.push((to, t));
            }
        }
    }

    pub fn rec_item(&mut self, span: Span, item: Option<Item>) {
        if let Some(item) = item {
            self.rec_ref(span, Target::of_item(item));
        }
    }

    /// The expression / declaration at `span` has type `ty`.
    pub fn rec_ty(&mut self, span: Span, ty: TyId) {
        if self.ide.is_none() {
            return;
        }
        let ctx = self.display_ctx();
        if let Some(r) = &mut self.ide {
            r.types.push((span, ty, ctx));
        }
    }

    /// The current display context (`display_params`), interned.
    pub fn display_ctx(&mut self) -> u32 {
        let Some(r) = &mut self.ide else { return 0 };
        if let Some(i) = r.params.iter().rposition(|p| *p == self.display_params) {
            return i as u32;
        }
        r.params.push(self.display_params.clone());
        (r.params.len() - 1) as u32
    }
}

impl FnCx<'_, '_> {
    /// The declaring identifier of local `l` of the current frame (following captures out to
    /// the enclosing function that declared it).
    fn local_origin(&self, l: LocalId) -> (Span, bool, bool) {
        let mut depth = self.outer.len();
        let mut l = l;
        loop {
            let frame = if depth == self.outer.len() {
                &self.f
            } else {
                &self.outer[depth]
            };
            let kind = frame.kinds[l.0 as usize];
            if kind == LocalKind::Capture && depth > 0 {
                if let Some(c) = frame.captures.iter().find(|c| c.inner == l) {
                    l = c.outer;
                    depth -= 1;
                    continue;
                }
            }
            let def = &frame.locals[l.0 as usize];
            let is_param = matches!(kind, LocalKind::Param | LocalKind::This);
            return (def.span, is_param, def.mutable);
        }
    }

    /// The type to show for a local declared at `decl` (a closure keeps its param names).
    fn shown_local_ty(&mut self, decl: Span, ty: TyId) -> TyId {
        match self
            .cx
            .ide
            .as_ref()
            .and_then(|r| r.closures.get(&decl))
            .copied()
        {
            Some(d) => self.cx.ty.intern(TyKind::Closure(d)),
            None => ty,
        }
    }

    /// The type to show for checked expression `h`.
    pub(crate) fn shown_ty(&mut self, h: &crate::hir::Expr) -> TyId {
        match h.kind {
            H::Closure(d) | H::FnRef(d, _) => self.cx.ty.intern(TyKind::Closure(d)),
            H::Local(l, _) => {
                let decl = self.local_origin(l).0;
                self.shown_local_ty(decl, h.ty)
            }
            _ => h.ty,
        }
    }

    /// `const <decl> = <closure>` (before the local is declared).
    pub(crate) fn rec_closure_decl(&mut self, decl: Span, init: &crate::hir::Expr) {
        if let (H::Closure(d), Some(r)) = (&init.kind, &mut self.cx.ide) {
            r.closures.insert(decl, *d);
        }
    }

    pub(crate) fn local_target(&mut self, l: LocalId) -> Target {
        let (decl, is_param, mutable) = self.local_origin(l);
        let def = &self.f.locals[l.0 as usize];
        let (name, ty) = (def.name.clone(), def.ty);
        let ty = self.shown_local_ty(decl, ty);
        let ctx = self.cx.display_ctx();
        Target::Local(LocalTarget {
            decl,
            name,
            ty,
            ctx,
            is_param,
            mutable,
        })
    }

    /// `span` uses local `l`.
    pub(crate) fn rec_local(&mut self, span: Span, l: LocalId) {
        if self.cx.recording() {
            let t = self.local_target(l);
            self.cx.rec_ref(span, t);
        }
    }

    /// Local `l` was declared by the identifier at its span.
    pub(crate) fn rec_local_decl(&mut self, l: LocalId) {
        if self.cx.recording() {
            let def = &self.f.locals[l.0 as usize];
            let (span, ty) = (def.span, def.ty);
            self.rec_local(span, l);
            let ty = self.shown_local_ty(span, ty);
            self.cx.rec_ty(span, ty);
        }
    }

    /// Scope `s` of the current frame ends: its locals were visible up to `s.hi`.
    pub(crate) fn rec_scope(&mut self, s: &Scope) {
        if !self.cx.recording() || s.hi == 0 {
            return;
        }
        for (name, &l) in &s.names {
            let kind = self.f.kinds[l.0 as usize];
            if kind == LocalKind::Capture || kind == LocalKind::Temp || name.starts_with('<') {
                continue;
            }
            let target = self.local_target(l);
            let decl = self.f.locals[l.0 as usize].span;
            if let Some(r) = &mut self.cx.ide {
                r.scopes.push(ScopeEntry {
                    file: decl.file,
                    lo: decl.lo,
                    hi: s.hi,
                    name: name.clone(),
                    target,
                });
            }
        }
    }

    /// Every scope still open in the current frame ends (the frame is finished).
    pub(crate) fn rec_frame_scopes(&mut self) {
        if !self.cx.recording() {
            return;
        }
        let scopes = std::mem::take(&mut self.f.scopes);
        for s in &scopes {
            self.rec_scope(s);
        }
        self.f.scopes = scopes;
    }
}
