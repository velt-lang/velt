//! Which site a function value is stored into, and where the values of a site go
//! (`fn_types`, docs/reference/errors.md "Dynamic calls").
//!
//! A closure created where a value of a site's type is stored (an argument for a parameter,
//! the initializer of a typed variable, a returned value, a field's value) throws into that
//! site only. A value read from one site and stored into another (an argument read from a
//! parameter) is a flow: what is stored into the first site is stored into the second. A
//! call through a value read from a site throws what is stored into that site. Where the
//! site is not known, the sites are matched by type, as before.

use velt_common::Span;

use super::fn_types::Portable;
use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};

impl Ctx<'_> {
    /// The type of the parameter or field named at `name` is written at `ty`.
    pub(crate) fn note_written_type(&mut self, name: Span, ty: Span) {
        if name != Span::DUMMY && ty != Span::DUMMY {
            self.fn_sites.written.insert(name, ty);
        }
    }

    /// The site written within `range` (the outermost one) whose type is `t`'s function type,
    /// or its promise type (`promise_sites`).
    pub(crate) fn site_in(&self, range: Span, t: TyId) -> Option<Span> {
        let t = self.site_core(t);
        if !matches!(self.ty.kind(t), TyKind::FnPtr { .. } | TyKind::Promise(..)) {
            return None;
        }
        let ids = self.fn_sites.by_file.get(&range.file)?;
        let mut best: Option<Span> = None;
        for &i in ids {
            let (s, g) = self.fn_sites.sites[i];
            if g != t || s.lo < range.lo || s.hi > range.hi {
                continue;
            }
            if best.is_none_or(|b| s.hi - s.lo > b.hi - b.lo) {
                best = Some(s);
            }
        }
        best
    }

    /// The site of the parameter, field or variable declared at `name` with type `t`.
    pub(crate) fn decl_site(&self, name: Span, t: TyId) -> Option<Span> {
        // A declaration's type is written in its own file, which must have sites.
        if !self.fn_sites.by_file.contains_key(&name.file) {
            return None;
        }
        match self.fn_sites.written.get(&name) {
            Some(&w) => self.site_in(w, t),
            // `let f = () => {}`: the site is the name (`untyped_let_site`).
            None => self.site_in(name, t),
        }
    }

    /// The site a value of type `to` being stored is stored into, if it is known.
    pub(crate) fn target_site(&self, to: TyId) -> Option<Span> {
        let s = self.fn_sites.target?;
        let to = self.site_core(to);
        let typed = (self.fn_sites.by_file.get(&s.file).into_iter().flatten())
            .any(|&i| self.fn_sites.sites[i] == (s, to));
        typed.then_some(s)
    }

    /// A closure created at `span` where a value of type `exp` is stored into the current
    /// target site: it throws into that site.
    pub(crate) fn note_closure_site(&mut self, span: Span, exp: Option<TyId>) {
        let Some(site) = exp.and_then(|t| self.target_site(t)) else {
            return;
        };
        self.fn_sites.closure_sites.insert(span, site);
    }

    /// The site closure `d` was created for.
    pub(crate) fn closure_site(&self, d: DefId) -> Option<Span> {
        let span = self.fn_info(d).span;
        self.fn_sites.closure_sites.get(&span).copied()
    }

    /// Values read from site `from` are stored into site `to`.
    pub(crate) fn site_flow(&mut self, from: Span, to: Span) {
        if from == to {
            return;
        }
        let out = self.fn_sites.flows.entry(from).or_default();
        if out.contains(&to) {
            return;
        }
        out.push(to);
        let set = (self.fn_sites.found.sites.get(&from).cloned()).unwrap_or_default();
        if !set.is_empty() {
            self.add_to_site(to, &set);
        }
    }

    /// Add `members` to what site `s` throws, and to the sites its values flow to.
    /// A generic closure throwing `err` is stored into a generic function type written without
    /// `throws` at `spans` (`crate::generic_fns`), which does not allow it: add `err` to those
    /// sites, as [`Ctx::seed_fn_sites`] does for a function type. False when `err` can't be
    /// kept (then the use is reported).
    pub(crate) fn seed_generic_sites(&mut self, spans: &[Span], err: TyId) -> bool {
        // An error-set variable (a call of a function-typed parameter) is only solved after
        // the bodies: check again with the sets written in, where it is a set.
        if self.mentions_err_var(err) {
            if self.fn_sites.is_last() {
                return false;
            }
            self.fn_sites.fallback();
            return true;
        }
        let mut members = vec![];
        for m in self.error_members(err) {
            match self.port_thrown(m) {
                Some(p) => members.push(p),
                None => return false,
            }
        }
        if spans.is_empty() || self.fn_sites.is_last() {
            return false;
        }
        for &s in spans {
            self.add_to_site(s, &members);
        }
        true
    }

    pub(super) fn add_to_site(&mut self, s: Span, members: &[Portable]) {
        let mut work = vec![s];
        while let Some(s) = work.pop() {
            let set = self.fn_sites.found.sites.entry(s).or_default();
            let mut grew = false;
            for p in members {
                if !set.contains(p) {
                    set.push(p.clone());
                    grew = true;
                    self.fn_sites.added += 1;
                }
            }
            if grew {
                self.fn_sites.grew = true;
                self.fn_sites.indirect.clear();
                self.fn_sites.indirect_sites.clear();
                work.extend(self.fn_sites.flows.get(&s).into_iter().flatten().copied());
            }
        }
    }

    /// What is stored into site `s`, as found so far.
    pub(super) fn site_set(&mut self, s: Span) -> Option<TyId> {
        if let Some(e) = self.fn_sites.indirect_sites.get(&s) {
            return *e;
        }
        let ps = (self.fn_sites.found.sites.get(&s).cloned()).unwrap_or_default();
        let ms: Vec<TyId> = ps.iter().filter_map(|p| self.unport(p)).collect();
        let e = self.error_union(&ms);
        self.fn_sites.indirect_sites.insert(s, e);
        e
    }
}

impl crate::body::FnCx<'_, '_> {
    /// The site the value of `h` is read from: a parameter, a variable or a field whose
    /// written type is a site.
    pub(crate) fn site_of_value(&self, h: &crate::hir::Expr) -> Option<Span> {
        use crate::hir::ExprKind as H;
        match &h.kind {
            H::Local(l, _) => self.local_site(*l),
            H::Global(d) => match self.cx.info.get(d.0 as usize) {
                Some(crate::defs::DefInfo::Global(g)) => self.cx.decl_site(g.span, g.ty),
                _ => None,
            },
            H::Field { base, index, .. } => {
                let TyKind::Adt(d, _) = self.cx.ty.kind(base.ty) else {
                    return None;
                };
                let f = self.cx.adt(*d)?.fields.get(*index as usize)?;
                let anon = self.cx.fn_sites.anon_written.get(&(*d, *index)).copied();
                (self.cx.decl_site(f.span, f.declared)).or_else(|| self.cx.site_in(anon?, h.ty))
            }
            H::Call {
                callee: crate::hir::Callee::Def(g, _),
                ..
            } => {
                let f = self.cx.fn_info(*g);
                self.cx.site_in(f.ret_span?, f.ret)
            }
            _ => None,
        }
    }

    /// The site of the result type of the declared function whose body is being checked.
    pub(crate) fn return_site(&self, ret: TyId) -> Option<Span> {
        if self.cx.fn_sites.by_file.is_empty() {
            return None;
        }
        // A closure's or nested function's own written result.
        let rs = match self.outer.is_empty() {
            true => self.cx.fn_info(self.body_def?).ret_span?,
            false => self.f.ret_span?,
        };
        self.cx.site_in(rs, ret)
    }

    /// The site of local `l` (through the captures of the enclosing closures).
    fn local_site(&self, l: crate::hir::LocalId) -> Option<Span> {
        let mut frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        let mut f = frames.next()?;
        let mut l = l;
        for _ in 0..64 {
            let i = l.0 as usize;
            let def = f.locals.get(i)?;
            if f.kinds.get(i) != Some(&crate::body::LocalKind::Capture) {
                return self.cx.decl_site(def.span, def.ty);
            }
            l = f.captures.iter().find(|c| c.inner == l)?.outer;
            f = frames.next()?;
        }
        None
    }

    /// Store `h` (already checked) into site `to`: values read from its own site flow there.
    pub(crate) fn flow_into(&mut self, h: &crate::hir::Expr, to: Option<Span>) {
        if let (Some(to), Some(from)) = (to, self.site_of_value(h)) {
            self.cx.site_flow(from, to);
            // An array, `Set` or `Map` of promises is shared: what is stored through either name
            // is in both (`promise_sites`).
            let t = self.cx.ty.core(h.ty).unwrap_or(h.ty);
            if self.cx.site_core(t) != t {
                self.cx.site_flow(to, from);
            }
        }
    }

    /// The value `check` checks, stored into `site`: the target of a closure created for it,
    /// and of the values read from another site.
    pub(crate) fn stored_into(
        &mut self,
        site: Option<Span>,
        check: impl FnOnce(&mut Self) -> crate::hir::Expr,
    ) -> crate::hir::Expr {
        let outer = self.set_target(site);
        let h = check(self);
        self.set_target(outer);
        self.flow_into(&h, site);
        h
    }

    /// Make `site` the target of the value checked next, returning the previous target.
    pub(crate) fn set_target(&mut self, site: Option<Span>) -> Option<Span> {
        std::mem::replace(&mut self.cx.fn_sites.target, site)
    }

    /// The site the argument at `arg` for parameter `p` is stored into: the parameter's, or
    /// for the value of a `push`, the site of its array's element type.
    pub(crate) fn arg_site(&mut self, p: &crate::defs::ParamSig, arg: Span) -> Option<Span> {
        let pushed = (self.cx.fn_sites.push).filter(|(a, _)| *a == arg);
        let site = self.param_site(p).or(pushed.map(|(_, s)| s));
        if let Some(s) = site {
            self.cx.bind_async_arg(arg, s);
        }
        site
    }

    /// The site of parameter `p` of a function being called.
    pub(crate) fn param_site(&self, p: &crate::defs::ParamSig) -> Option<Span> {
        if !self.cx.fn_sites.by_file.contains_key(&p.span.file) {
            return None;
        }
        self.cx.decl_site(p.span, p.ty)
    }
}
