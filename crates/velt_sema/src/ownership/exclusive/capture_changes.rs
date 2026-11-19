//! Whether a closure modifies what it captured of an enclosing variable: through the captured
//! name, or any name its body gives (a part of) it.

use rustc_hash::FxHashSet;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, PatKind, Stmt,
    StmtKind as S, TyKind, UseMode,
};
use crate::visit::{self, VisitMut};

/// Does closure `c` modify or take over what it captured of enclosing variable `outer`? Through
/// the captured name or any name for (a part of) it: a binding initialized from it (`const a =
/// cap`, `const row = cap[0]`, destructuring), the elements of a loop or the bindings of a
/// `match` over it (unless each is an object of its own: a class instance, an interface or
/// `any` value), a share of it passed anywhere, or a closure it creates. Unknown cases count as
/// modifying.
pub(super) fn modifies_capture(cx: &Ctx, c: DefId, outer: LocalId, depth: u32) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return true;
    };
    let Some(cap) = f.captures.iter().find(|k| k.outer == outer) else {
        return false;
    };
    if depth > 8 {
        return true;
    }
    let mut block = f.body.block.clone();
    // The names for the capture, until no binding adds one.
    let mut aliases = Aliasing {
        cx,
        f,
        names: FxHashSet::from_iter([cap.inner]),
        inits: FxHashSet::default(),
        grew: false,
    };
    loop {
        aliases.grew = false;
        visit::block(&mut block, &mut aliases);
        if !aliases.grew {
            break;
        }
    }
    if aliases
        .names
        .iter()
        .any(|l| f.body.locals[l.0 as usize].mutable)
    {
        return true;
    }
    let mut scan = Changes {
        cx,
        names: &aliases.names,
        inits: &aliases.inits,
        depth,
        hit: false,
    };
    visit::block(&mut block, &mut scan);
    scan.hit
}

/// The local at the root of place `e`, through shares and conversions (`None` for a value that
/// is not a place, or a copy: `clone()`).
pub(super) fn alias_root(e: &Expr) -> Option<LocalId> {
    let mut x = e;
    loop {
        match &x.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => x = &args[0],
            E::Upcast(inner) | E::Downcast(inner) | E::WrapSome(inner) | E::Cast(inner) => {
                x = inner
            }
            E::Field { base, .. }
            | E::Index { base, .. }
            | E::UnwrapSome(base, _)
            | E::UnwrapVariant { expr: base, .. } => x = base,
            E::Local(l, _) => return Some(*l),
            _ => return None,
        }
    }
}

/// Collects the names for a capture (`modifies_capture`).
struct Aliasing<'a, 'm> {
    cx: &'a Ctx<'m>,
    f: &'a FnDef,
    names: FxHashSet<LocalId>,
    /// Spans of the initializers that bind a name (a share there is that name's).
    inits: FxHashSet<Span>,
    grew: bool,
}

impl Aliasing<'_, '_> {
    fn from(&mut self, init: &Expr) -> bool {
        alias_root(init).is_some_and(|l| self.names.contains(&l))
    }

    fn bind(&mut self, l: LocalId) {
        let ty = self.f.body.locals[l.0 as usize].ty;
        let own = self.cx.class_of(ty).is_some() || matches!(self.cx.ty.kind(ty), TyKind::Dyn(..));
        if !own && self.names.insert(l) {
            self.grew = true;
        }
    }

    fn bind_pat(&mut self, p: &Pat) {
        let mut ls = vec![];
        pat_locals(p, &mut ls);
        for l in ls {
            self.bind(l);
        }
    }
}

impl VisitMut for Aliasing<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } if self.from(init) => {
                self.inits.insert(init.span);
                self.bind(*local);
            }
            S::LetPat { pat, init } if self.from(init) => {
                self.inits.insert(init.span);
                self.bind_pat(pat);
            }
            S::ForOf { binding, iter, .. } if self.from(iter) => self.bind_pat(binding),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let E::Match { scrutinee, arms } = &e.kind {
            if self.from(scrutinee) {
                for a in arms {
                    self.bind_pat(&a.pat);
                }
            }
        }
    }
}

/// Finds a change through one of the names (`modifies_capture`).
struct Changes<'a, 'm> {
    cx: &'a Ctx<'m>,
    names: &'a FxHashSet<LocalId>,
    inits: &'a FxHashSet<Span>,
    depth: u32,
    hit: bool,
}

impl VisitMut for Changes<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        let named = |x: &Expr| alias_root(x).is_some_and(|l| self.names.contains(&l));
        match &e.kind {
            E::Local(l, UseMode::BorrowMut | UseMode::Move) if self.names.contains(l) => {
                self.hit = true
            }
            E::Field {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            }
            | E::Index {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            }
            | E::UnwrapSome(_, UseMode::BorrowMut | UseMode::Move)
            | E::UnwrapVariant {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            } if named(e) => self.hit = true,
            // Another reference to it, kept anywhere but a name the scan follows.
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                ..
            } if named(e) && !self.inits.contains(&e.span) => self.hit = true,
            E::Closure(nc) => {
                for &l in self.names.iter() {
                    if modifies_capture(self.cx, *nc, l, self.depth + 1) {
                        self.hit = true;
                    }
                }
            }
            _ => {}
        }
    }
}

/// The variables pattern `p` binds.
fn pat_locals(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, _) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|p| pat_locals(p, out))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|p| pat_locals(p, out)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| pat_locals(p, out)),
        PatKind::Some(p) => pat_locals(p, out),
        _ => {}
    }
}
