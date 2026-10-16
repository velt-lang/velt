//! Recursive closure groups (docs/reference/functions.md "Declarations"): nested functions
//! that use the enclosing function's locals and call each other (or themselves), made closure
//! constants by `crate::nested_closures`, which records each group (`ClosureGroup`).
//!
//! The members are checked together. At the first member's `const`, every member's local is
//! declared with its function type, so each body can name the others; the error types come
//! from trial checks repeated until they no longer change (a member's error type includes what
//! the members it calls throw). Each member captures every variable of the group's captures
//! first, in the same order (its own uses, or not), so all members' environments have one
//! layout. Inside a member, another member (or itself) is a hidden local of the member's body
//! holding `Intrinsic::ClosureSibling(def)`: that member's code with the running closure's own
//! environment. No closure refers to its own environment, so a group that escapes (returned,
//! stored) holds no reference cycle: the environment is freed with the last member value.

use std::collections::HashMap;

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, Frame, LocalKind};
use crate::ctx::Ctx;
use crate::hir::{self, DefId, Intrinsic, LocalDef, LocalId, TyId};

/// A member of a group declared in a frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Member {
    pub def: DefId,
    pub ty: TyId,
    pub local: LocalId,
}

/// A frame's part in recursive closure groups.
#[derive(Clone, Debug, Default)]
pub(crate) struct GroupFrame {
    /// The members of the groups declared in this frame, by group.
    pub members: HashMap<usize, Vec<Member>>,
    /// Group and member of each member's local.
    pub locals: HashMap<LocalId, (usize, usize)>,
    /// This frame checks member (group, member)'s body.
    pub member_of: Option<(usize, usize)>,
    /// Hidden locals of this member's body holding other members (or itself): local,
    /// definition, type.
    pub peers: Vec<(LocalId, DefId, TyId)>,
}

impl Ctx<'_> {
    /// Install the groups `crate::nested_closures` made.
    pub(crate) fn set_closure_groups(&mut self, groups: &[crate::nested_closures::ClosureGroup]) {
        self.closure_groups = groups.to_vec();
        self.group_names.clear();
        for (g, grp) in groups.iter().enumerate() {
            for (m, s) in grp.name_spans.iter().enumerate() {
                self.group_names.insert(*s, (g, m));
            }
        }
    }
}

impl FnCx<'_, '_> {
    /// `const name = (…) => …;` of a group member: checked as described in the module docs.
    /// False when `name` is no member.
    pub(super) fn group_member_decl(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let Some(&(g, m)) = self.cx.group_names.get(&name.span) else {
            return false;
        };
        let Some(e) = v.init.as_ref() else {
            return false;
        };
        if !self.f.groups.members.contains_key(&g) {
            self.start_group(g);
        }
        let mem = self.f.groups.members[&g][m];
        self.pending_member = Some((g, m, mem.def));
        let h = self.closure(e, Some(mem.ty), true);
        self.pending_member = None;
        out.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: mem.local,
                init: Some(h),
            },
            span,
        });
        true
    }

    /// Declare group `g`'s members and settle their function types (module docs).
    fn start_group(&mut self, g: usize) {
        let grp = self.cx.closure_groups[g].clone();
        let err = self.cx.ty.error;
        let mut members = vec![];
        for (m, name) in grp.names.iter().enumerate() {
            let def = self.alloc_closure(grp.arrow_spans[m]);
            self.cx.group_defs.insert(def);
            let ident = ast::Ident {
                name: name.clone(),
                span: grp.name_spans[m],
            };
            let local = self.declare_local(&ident, err, LocalKind::Const);
            self.f.groups.locals.insert(local, (g, m));
            self.f.closure_consts.insert(local, def);
            members.push(Member {
                def,
                ty: err,
                local,
            });
        }
        self.f.groups.members.insert(g, members);
        // Each round checks every member against the others' types of the round before.
        for _ in 0..=grp.names.len() + 1 {
            let mut changed = false;
            for (m, arrow) in grp.arrows.iter().enumerate() {
                let mark = super::recheck::Mark::here(self.cx);
                let frames = self.trial_frames();
                // The member's own definition: a trial leaves its parameters and their
                // defaults, which calls of it in the other members fill in.
                let def = self.f.groups.members[&g][m].def;
                self.pending_member = Some((g, m, def));
                let h = self.closure(arrow, None, true);
                self.pending_member = None;
                mark.rollback(self.cx);
                self.restore_trial_frames(frames);
                let mem = &mut self.f.groups.members.get_mut(&g).expect("ICE: group")[m];
                if h.ty != mem.ty && !self.cx.ty.has_error(h.ty) {
                    mem.ty = h.ty;
                    let l = mem.local;
                    self.f.locals[l.0 as usize].ty = h.ty;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// At the start of member (g, m)'s closure frame: mark it, and capture the group's
    /// variables in their order. The captures stay in the frame's outermost scope; the
    /// parameters and the body get a scope of their own inside it, so they may declare names
    /// the group captures (only the member's own uses of such a name reach the capture).
    pub(super) fn enter_group_member(&mut self, g: usize, m: usize, span: Span) {
        self.f.groups.member_of = Some((g, m));
        let names = self.cx.closure_groups[g].captures.clone();
        for n in &names {
            self.lookup_local(n, span);
        }
        self.push_scope();
    }

    /// `prev`, a local of frame `j` found for `name`: when it is a group member and the frame
    /// right inside `j` checks a member of that group, the hidden local of that member's
    /// frame holding it (made on first use) and that frame's index (`self.outer.len()` for the
    /// current frame).
    pub(super) fn group_peer(
        &mut self,
        j: usize,
        prev: LocalId,
        name: &str,
        span: Span,
    ) -> Option<(usize, LocalId)> {
        let &(g, q) = self.outer[j].groups.locals.get(&prev)?;
        let mem = self.outer[j].groups.members.get(&g)?[q];
        let k = j + 1;
        let fr: &mut Frame = if k == self.outer.len() {
            &mut self.f
        } else {
            &mut self.outer[k]
        };
        if fr.groups.member_of.map(|(gg, _)| gg) != Some(g) {
            return None;
        }
        if let Some(&(l, _, _)) = fr.groups.peers.iter().find(|p| p.1 == mem.def) {
            return Some((k, l));
        }
        let l = LocalId(fr.locals.len() as u32);
        fr.locals.push(LocalDef {
            name: name.to_string(),
            ty: mem.ty,
            mutable: false,
            boxed: false,
            span,
        });
        fr.kinds.push(LocalKind::Const);
        fr.scopes[0].names.insert(name.to_string(), l);
        fr.closure_consts.insert(l, mem.def);
        fr.groups.peers.push((l, mem.def, mem.ty));
        Some((k, l))
    }

    /// The `let`s of a member's hidden locals (`frame` is the member's frame), first in its body.
    pub(super) fn group_peer_lets(&mut self, frame: &Frame, span: Span) -> Vec<hir::Stmt> {
        frame
            .groups
            .peers
            .iter()
            .map(|&(local, def, ty)| {
                let init = self.mk(
                    hir::ExprKind::Call {
                        callee: hir::Callee::Intrinsic(Intrinsic::ClosureSibling(def)),
                        args: vec![],
                    },
                    ty,
                    span,
                );
                hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local,
                        init: Some(init),
                    },
                    span,
                }
            })
            .collect()
    }
}
