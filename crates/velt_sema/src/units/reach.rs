//! EXPERIMENT (#2943 M3): the definitions a build from std's unit on disk lowers, at most: the
//! program is built from these, std's others are placeholders never decoded. A superset of what
//! lowering builds, as lazy std's live set is (`body::lazy_std::live`), and handed to lowering as
//! `hir::Program::live`, so that lowering asserts each function it builds is in it
//! (`Cx::assert_live`, a debug compiler): a rule missing here fails there, loudly.
//!
//! The roots: the program's own definitions (and its dependencies', `units::libraries`), every
//! item of the std modules lowering looks into by name and of the prelude and the hooks
//! (lazy std's `BY_NAME` and `HELPERS`, taken whole), every hook's module, the module slots and
//! the entry. Closed over: what a body or a module constant's initializer names (a call, a
//! function value, a closure, a global, a class built or matched), the types its expressions
//! and signatures mention, and for each class or struct of those types its methods, statics,
//! constructor, vtable, base, fields' types and defaults and its subclasses, for each enum its
//! payloads, and for each interface its implementations (and the implementations of every live
//! type).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};

/// std's roots ([`self`]): the std functions lowering looks up by name, as lazy std's roots
/// (`Mode::named`), and every hook (lazy std demands those the program's constructs need: all
/// is a superset). They depend on std alone, so std's unit keeps them
/// (`Summaries::reach_roots`, made as the unit is finished) instead of every build looking at
/// each item of each std module by its qualified name.
pub(crate) fn std_roots(cx: &Ctx) -> Vec<DefId> {
    let mut roots = vec![];
    for m in 0..cx.modules.len().min(cx.scopes.len()) {
        let module = cx.modules[m];
        if !module.is_std {
            continue;
        }
        // The hooks' modules whole: their code reaches functions by name (`hookCall`'s stubs).
        let hooks_dir = module.path.starts_with(velt_common::std_dir!(lang_hooks));
        let named = match hooks_dir {
            true => Some(true),
            false => crate::body::lazy_std::named_module(&module.path),
        };
        for (name, item) in &cx.scopes[m].items {
            let Item::Def(d) = item else { continue };
            let qualified = format!("{}::{name}", module.path);
            let root = match named {
                Some(true) => true,
                Some(false) => crate::body::lazy_std::is_helper(&qualified),
                None => false,
            };
            let hook = velt_common::std_hooks::find(&qualified).is_some()
                || crate::body::lazy_std::SCHED_HOOKS.contains(&qualified.as_str());
            if root || hook {
                roots.push(*d);
            }
        }
    }
    roots
}

/// The definitions a build from std's unit on disk is made of ([`self`]).
pub(crate) fn program_set(cx: &mut Ctx, entry: Option<DefId>) -> FxHashSet<DefId> {
    let base = cx.unit_sums.lib_base.min(cx.unit_base as usize);
    let base = if crate::units::libraries() {
        base
    } else {
        cx.unit_base as usize
    };
    let mut todo: Vec<DefId> = (base..cx.info.len()).map(|i| DefId(i as u32)).collect();
    todo.extend(entry);
    todo.extend(cx.module_slots.iter().copied());
    // std's roots depend on std alone: its unit keeps them ([`std_roots`]).
    match &cx.unit_sums.reach_roots {
        Some(roots) => todo.extend(roots.iter().copied()),
        None => todo.extend(std_roots(cx)),
    }
    let mut walk = Walk {
        live: FxHashSet::default(),
        todo,
        types: vec![],
        seen_types: FxHashSet::default(),
        ifaces: FxHashSet::default(),
        adts: FxHashSet::default(),
        catches: false,
    };
    let below = cx.subclass_map().clone();
    loop {
        while let Some(d) = walk.todo.pop() {
            if (d.0 as usize) >= cx.info.len() || !walk.live.insert(d) {
                continue;
            }
            walk.def(cx, d);
        }
        while let Some(t) = walk.types.pop() {
            walk.ty(cx, t);
        }
        // Subclasses of the live classes (a class value's glue dispatches over them) and the
        // implementations of live interfaces and of live types.
        let adts: Vec<DefId> = walk.adts.iter().copied().collect();
        for a in adts {
            for sub in below.get(&a).map(|v| v.as_slice()).unwrap_or(&[]) {
                if !walk.live.contains(sub) {
                    walk.todo.push(*sub);
                }
            }
        }
        for imp in cx.impls.iter() {
            let of_live_type = match cx.ty.kind(imp.ty) {
                TyKind::Adt(a, _) => walk.adts.contains(a),
                _ => walk.seen_types.contains(&imp.ty),
            };
            if walk.ifaces.contains(&imp.iface) || of_live_type {
                for m in &imp.methods {
                    if !walk.live.contains(m) {
                        walk.todo.push(*m);
                    }
                }
                if walk.seen_types.insert(imp.ty) {
                    walk.types.push(imp.ty);
                }
            }
        }
        if walk.todo.is_empty() && walk.types.is_empty() {
            break;
        }
    }
    if walk.catches {
        crate::body::lazy_std::note_catch_unwind(cx);
    }
    if std::env::var_os("VELT_UNITS_REACH_STATS").is_some() {
        let std_defs = (walk.live.iter()).filter(|d| (d.0 as usize) < base).count();
        let by_module = {
            let mut m: FxHashMap<&str, usize> = FxHashMap::default();
            for d in walk.live.iter().filter(|d| (d.0 as usize) < base) {
                let module = match &cx.info[d.0 as usize] {
                    DefInfo::Fn(f) => f.module,
                    DefInfo::Global(g) => g.module,
                    DefInfo::Adt(a) => a.module,
                    _ => continue,
                };
                *m.entry(cx.modules[module].path.as_str()).or_default() += 1;
            }
            let mut v: Vec<_> = m.into_iter().collect();
            v.sort_by_key(|a| std::cmp::Reverse(a.1));
            v.truncate(25);
            v
        };
        eprintln!(
            "velt-units: reach: {} defs ({std_defs} below the unit base {base}, of {})",
            walk.live.len(),
            cx.info.len()
        );
        for (m, n) in by_module {
            eprintln!("velt-units: reach:   {n:6} {m}");
        }
    }
    walk.live
}

struct Walk {
    live: FxHashSet<DefId>,
    todo: Vec<DefId>,
    types: Vec<TyId>,
    seen_types: FxHashSet<TyId>,
    ifaces: FxHashSet<DefId>,
    adts: FxHashSet<DefId>,
    /// A `catchUnwind` was reached: std's panic path is demanded.
    catches: bool,
}

impl Walk {
    fn push_ty(&mut self, t: TyId) {
        if self.seen_types.insert(t) {
            self.types.push(t);
        }
    }

    fn def(&mut self, cx: &Ctx, d: DefId) {
        let i = d.0 as usize;
        match &cx.info[i] {
            DefInfo::Fn(f) => {
                for p in &f.params {
                    self.push_ty(p.ty);
                }
                self.push_ty(f.ret);
                if let Some(owner) = f.owner {
                    self.todo.push(owner);
                }
            }
            DefInfo::Global(g) => {
                self.push_ty(g.ty);
                if let Some(init) = &g.init {
                    crate::visit_ref::exprs_in(init, &mut |e| self.expr(e));
                }
            }
            DefInfo::Adt(a) => {
                self.adts.insert(d);
                self.todo.extend(a.methods.values().map(|m| m.def));
                self.todo.extend(a.statics.values().copied());
                self.todo.extend(a.ctor);
                self.todo.extend(a.vtable.iter().copied());
                if let Some(b) = a.base {
                    self.push_ty(b);
                }
                for f in &a.fields {
                    self.push_ty(f.ty);
                    if let Some(e) = &f.default {
                        crate::visit_ref::exprs_in(e, &mut |e| self.expr(e));
                    }
                }
                for r in a.redefined.iter().filter_map(|r| r.init.as_ref()) {
                    crate::visit_ref::exprs_in(r, &mut |e| self.expr(e));
                }
            }
            DefInfo::Enum(e) => {
                for v in &e.variants {
                    for &p in &v.payload {
                        self.push_ty(p);
                    }
                }
            }
            DefInfo::Iface(f) => {
                self.ifaces.insert(d);
                for fi in &f.fields {
                    self.push_ty(fi.ty);
                }
            }
        }
        if let Some(Def::Fn(f)) = &cx.defs[i] {
            for l in &f.body.locals {
                self.push_ty(l.ty);
            }
            crate::visit_ref::exprs(&f.body.block, &mut |e| self.expr(e));
        }
    }

    fn expr(&mut self, e: &Expr) {
        self.push_ty(e.ty);
        match &e.kind {
            E::Call {
                callee: Callee::Def(d, targs),
                ..
            }
            | E::FnRef(d, targs) => {
                self.todo.push(*d);
                for &t in targs {
                    self.push_ty(t);
                }
            }
            E::Call {
                callee: Callee::ParamMethod { iface, .. },
                ..
            } => {
                self.ifaces.insert(*iface);
                self.todo.push(*iface);
            }
            E::Call {
                callee:
                    Callee::Intrinsic(
                        crate::hir::Intrinsic::CatchUnwind | crate::hir::Intrinsic::CatchUnwindAt,
                    ),
                ..
            } => self.catches = true,
            E::Global(d) | E::Closure(d) => self.todo.push(*d),
            E::AdtLit { def, .. } | E::Variant { def, .. } | E::New { def, .. } => {
                self.todo.push(*def)
            }
            _ => {}
        }
    }

    fn ty(&mut self, cx: &Ctx, t: TyId) {
        let kind = cx.ty.kind(t).clone();
        match &kind {
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) => {
                self.todo.push(*d);
                if matches!(kind, TyKind::Dyn(..)) {
                    self.ifaces.insert(*d);
                }
            }
            TyKind::Closure(d) => self.todo.push(*d),
            _ => {}
        }
        for c in crate::types::children(&kind) {
            self.push_ty(c);
        }
    }
}
