//! A kept check (docs/internals/design/dev-warm-front-end.md step 3, "C2"): a clone of a check's
//! context, made before the finishing steps consume it, together with everything it borrows,
//! for the dev host to keep between saves and check edited bodies again in.
//!
//! A `Ctx<'m>` borrows the modules it checks for `'m`: the driver's parsed modules, or the
//! source rewrites' copy of them (`check_timed`), and the std modules loaded on demand, which
//! the driver's `StdLoads` holds. A kept check owns all of them. The check hands over the clone
//! and the rewritten modules it borrows as a [`PendingKept`]; the driver then gives it its parsed
//! modules and its `StdLoads` ([`PendingKept::adopt`]), which checks, by their heap addresses,
//! that they are the very ones the clone borrows, and only then is there a [`KeptCheck`].
//!
//! The clone shares no generator and loads no std module (`gen.generate` and `loads` are taken
//! out): a body checked again that would generate code or load std falls back to the cold
//! check. A check that added generated modules, or that checked another copy of the modules than
//! the driver's (a script whose variables `script_locals` moved into `main`), keeps nothing.
//!
//! Safety, in one place: the clone's `'m` is erased to `'static`. That is sound because
//! everything it borrows is owned by the same value and kept at the same heap address (the
//! buffers of `Vec`s and the contents of `Box`es move with their owner without moving), and
//! the clone is dropped first ([`Owned`]'s `Drop`). Dropping a `Ctx` reads nothing through its
//! borrows (they are plain references and the maps hold them as keys), so dropping a
//! `PendingKept` that was never adopted, after the driver's modules are gone, is sound too. A
//! kept check crosses from sema's thread to the driver's: every `Rc` in the clone is shared only
//! with the context it was cloned from, which is dropped on sema's thread before the thread is
//! joined (the join orders that drop before any use on the driver's side).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;

use crate::ctx::Ctx;
use crate::hir::DefId;
pub(crate) use crate::kept_reload::Facts;
use crate::kept_reload::{decl_at, decl_path, find_decl, hash_of, others_stay, signature};
use crate::std_load::StdLoads;
use crate::SourceModule;

/// The heap address and length of a module slice: what a kept check checks the driver's
/// modules against.
type Place = (usize, usize);

fn place(modules: &[SourceModule]) -> Place {
    match modules.len() {
        // An empty slice's address means nothing: every empty one is the same place.
        0 => (0, 0),
        n => (modules.as_ptr() as usize, n),
    }
}

thread_local! {
    /// A reload's question for `check_timed` (`probe`), and the answer: what the source rewrites
    /// and scans find of the edited program.
    static PROBE: RefCell<(bool, Option<Facts>)> = const { RefCell::new((false, None)) };
    /// The check's context after its bodies were checked (`crate::analyze`), the last round's.
    static STASH: RefCell<Option<Detached>> = const { RefCell::new(None) };
    /// Set on sema's thread for one check: keep the check of exactly these modules.
    static REQUEST: Cell<Option<Place>> = const { Cell::new(None) };
    /// What the check kept, for [`take`].
    static OFFER: RefCell<Option<PendingKept>> = const { RefCell::new(None) };
}

/// Does `check_timed` need what the source rewrites and scans find (`Facts`): for a check that
/// keeps itself, or a reload's probe?
pub(crate) fn wants_facts(input: &[SourceModule]) -> bool {
    wanted(input) || PROBE.with(|p| p.borrow().0)
}

/// Called by `check_timed` once the source rewrites and scans are done: when a reload probes
/// (`probe`), records what they found and stops the check.
pub(crate) fn probed(facts: &Option<Facts>) -> bool {
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        if !p.0 {
            return false;
        }
        p.1 = facts.clone();
        true
    })
}

/// What the source rewrites and scans find of `modules`, asked of the very rewrites a check
/// runs (`check_timed`, stopped after them).
fn probe(modules: &[SourceModule], root: usize, opts: crate::CheckOptions) -> Option<Facts> {
    PROBE.with(|p| *p.borrow_mut() = (true, None));
    let _ = crate::check_timed(
        modules,
        root,
        opts,
        &[],
        None,
        None,
        crate::SEMA_STACK_BUDGET,
    );
    PROBE.with(|p| std::mem::take(&mut *p.borrow_mut()).1)
}

/// Keep a clone of `cx`, whose bodies are checked and whose later passes are yet to run, for a
/// reload to check edited bodies again from (the latest round's replaces an earlier one's).
pub(crate) fn stash(cx: &Ctx<'_>) {
    STASH.with(|s| *s.borrow_mut() = Some(detach(cx.clone())));
}

/// Ask the check about to run on this thread to keep itself, if it checks `modules`. The
/// request lasts while the guard returned lives: a check that panics leaves none behind.
pub(crate) fn request(modules: &[SourceModule]) -> Request {
    REQUEST.with(|r| r.set(Some(place(modules))));
    OFFER.with(|o| o.borrow_mut().take());
    STASH.with(|s| s.borrow_mut().take());
    Request
}

/// Clears this thread's request when dropped ([`request`]).
pub(crate) struct Request;

impl Drop for Request {
    fn drop(&mut self) {
        REQUEST.with(|r| r.set(None));
    }
}

/// What the check kept, and the request cleared.
pub(crate) fn take() -> Option<PendingKept> {
    REQUEST.with(|r| r.set(None));
    STASH.with(|s| s.borrow_mut().take());
    OFFER.with(|o| o.borrow_mut().take())
}

/// Does the check of `modules` keep itself?
pub(crate) fn wanted(modules: &[SourceModule]) -> bool {
    REQUEST.with(|r| r.get()) == Some(place(modules))
}

/// A clone whose borrows are no longer tracked: [`offer`] gives it the owners of what it
/// borrows, at the end of the check; dropped instead, it reads nothing through them.
pub(crate) struct Detached(Ctx<'static>);

/// The clone `cx` detached from the lifetime of what it borrows (module docs).
pub(crate) fn detach(cx: Ctx<'_>) -> Detached {
    // SAFETY: module docs. `offer` moves the owners of what it borrows into the same value, at
    // stable addresses; until then it is only dropped, which reads nothing through them.
    Detached(unsafe { std::mem::transmute::<Ctx<'_>, Ctx<'static>>(cx) })
}

/// A rewrite's modules, where the clone may borrow them: their address (if any) and their
/// owner, boxed.
pub(crate) type Stage = (Option<*const SourceModule>, Box<dyn Any>);

/// Offer the clone `cx` of the check of `input` and the generated `sources` given with it, whose
/// final modules are at `checked`: of the rewrite `stages`, the one holding them is kept (none
/// when `checked` is `input`). `loads`: the id of the driver's `StdLoads` the check borrowed
/// from, if any.
pub(crate) fn offer(
    Detached(mut cx): Detached,
    input: &[SourceModule],
    sources: &[SourceModule],
    checked: (*const SourceModule, usize),
    stages: Vec<Stage>,
    loads: Option<usize>,
) {
    let (checked, checked_len) = checked;
    cx.loads = None;
    cx.gen.generate = None;
    let stage = match checked == input.as_ptr() {
        true => None,
        false => match stages.into_iter().find(|(at, _)| *at == Some(checked)) {
            Some((_, owner)) => Some(owner),
            // ICE-free: a check whose modules nothing here owns keeps nothing.
            None => return,
        },
    };
    let ctx = cx;
    let before = STASH
        .with(|s| s.borrow_mut().take())
        .map(|Detached(mut b)| {
            b.loads = None;
            b.gen.generate = None;
            ManuallyDrop::new(Box::new(b))
        });
    let pending = PendingKept {
        owned: Owned {
            ctx: ManuallyDrop::new(Box::new(ctx)),
            before,
            stage,
            input: None,
            sources: None,
            loads: None,
        },
        input: place(input),
        sources: place(sources),
        checked: (checked as usize, checked_len),
        loads,
    };
    OFFER.with(|o| *o.borrow_mut() = Some(pending));
}

/// A kept check's clone and what it borrows, dropped in that order.
struct Owned {
    ctx: ManuallyDrop<Box<Ctx<'static>>>,
    /// The context after its bodies were checked, before the passes that follow (`stash`).
    before: Option<ManuallyDrop<Box<Ctx<'static>>>>,
    /// Held for the modules the context borrows from it.
    stage: Option<Box<dyn Any>>,
    input: Option<Vec<SourceModule>>,
    sources: Option<Vec<SourceModule>>,
    loads: Option<StdLoads>,
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: dropped once, here, before the fields they borrow (dropped after this).
        unsafe { ManuallyDrop::drop(&mut self.ctx) }
        if let Some(b) = &mut self.before {
            // SAFETY: as above.
            unsafe { ManuallyDrop::drop(b) }
        }
    }
}

/// A kept check that still borrows the driver's modules and std loads: [`PendingKept::adopt`]
/// takes them over.
pub struct PendingKept {
    owned: Owned,
    input: Place,
    sources: Place,
    /// Where the rewrites' copy the context checked is (in `owned.stage`, when it is one).
    checked: Place,
    loads: Option<usize>,
}

// SAFETY: module docs (the clone's `Rc`s were shared only with a context dropped before the
// check's thread was joined).
unsafe impl Send for PendingKept {}

impl PendingKept {
    /// The kept check, owning the modules the check was given (`input`, the driver's parsed
    /// modules or its copy of them), the generated `sources` given with them, and the std
    /// `loads`, if they are at the heap addresses the check borrowed; otherwise they come back.
    ///
    /// # Safety
    ///
    /// `input`, `sources` and `loads` must be the very values whose slices (and `StdLoads`) the
    /// check that made this was given, alive and unchanged since: not other values that happen to
    /// be at the same addresses, which the check can't tell apart (a vector dropped and another
    /// allocated in its place). The kept check reads through borrows into them.
    pub unsafe fn adopt(
        mut self,
        input: Vec<SourceModule>,
        sources: Vec<SourceModule>,
        loads: Option<StdLoads>,
    ) -> Result<KeptCheck, Box<Refused>> {
        let same_loads = match (self.loads, &loads) {
            (None, _) => true,
            (Some(id), Some(l)) => l.id() == id,
            (Some(_), None) => false,
        };
        if place(&input) != self.input || place(&sources) != self.sources || !same_loads {
            // Boxed: `Refused` holds three vectors' worth of values (over clippy's
            // `result_large_err` limit on some targets), and refusing is the rare path.
            return Err(Box::new(Refused {
                input,
                sources,
                loads,
            }));
        }
        // A context that checked the source rewrites' copy borrows nothing of `input` (each
        // rewrite copies every module): only the copy is kept, which halves what is kept.
        self.owned.input = self.owned.stage.is_none().then_some(input);
        self.owned.sources = Some(sources);
        self.owned.loads = loads;
        Ok(KeptCheck {
            owned: self.owned,
            checked: self.checked,
        })
    }
}

/// What [`PendingKept::adopt`] gives back when they are not what the check borrowed.
pub struct Refused {
    pub input: Vec<SourceModule>,
    pub sources: Vec<SourceModule>,
    pub loads: Option<StdLoads>,
}

/// A check kept by the dev host: the checking context of the running version, before its
/// finishing steps, with everything it borrows (module docs).
pub struct KeptCheck {
    owned: Owned,
    /// Where the rewrites' copy the context checked is, when `owned.stage` holds it.
    checked: Place,
}

// SAFETY: as for `PendingKept`.
unsafe impl Send for KeptCheck {}

impl KeptCheck {
    fn cx(&self) -> &Ctx<'static> {
        &self.owned.ctx
    }

    /// The modules the context checked, their paths in order (generated ones included).
    pub fn module_paths(&self) -> Vec<&str> {
        self.cx().modules.iter().map(|m| m.path.as_str()).collect()
    }

    /// The source file of each module, and whether it is std's, in the order of
    /// [`module_paths`](Self::module_paths).
    pub fn module_files(&self) -> Vec<(velt_common::FileId, bool)> {
        self.cx()
            .modules
            .iter()
            .map(|m| (m.file, m.is_std))
            .collect()
    }

    /// Does the context borrow the source rewrites' copy of the modules (which the kept check
    /// owns), rather than the driver's modules themselves?
    pub fn owns_rewritten_modules(&self) -> bool {
        self.owned.stage.is_some()
    }

    /// Does every module the context reads lie in a buffer this kept check owns: the modules
    /// it was given, the generated ones, the rewrites' copy it holds, or a std batch? A
    /// deterministic check of what the module docs argue (a kept check that lost an owner fails
    /// it, where reading through the dangling borrows may still seem to work).
    pub fn borrows_only_what_it_owns(&self) -> bool {
        let size = std::mem::size_of::<SourceModule>();
        let mut owned: Vec<Place> = vec![];
        owned.extend(self.owned.input.as_deref().map(place));
        owned.extend(self.owned.sources.as_deref().map(place));
        owned.extend(self.owned.stage.is_some().then_some(self.checked));
        owned.extend(
            self.owned
                .loads
                .as_ref()
                .map_or(vec![], |l| l.module_places()),
        );
        let inside =
            |at: usize| (owned.iter()).any(|&(start, n)| at >= start && at < start + n * size);
        let modules = &self.cx().modules;
        modules
            .iter()
            .all(|m| inside(*m as *const SourceModule as usize))
    }

    /// The number of the context's definitions.
    pub fn definitions(&self) -> usize {
        self.cx().defs.len()
    }

    /// The number of top-level items of each module, read through the context's borrows.
    pub fn items(&self) -> usize {
        self.cx().modules.iter().map(|m| m.ast.items.len()).sum()
    }

    /// The program again, with the bodies of the functions named `names` checked again (as
    /// they were: the fast path's reload of an edit, before it has edits), from the context kept
    /// after the bodies' check: those bodies' first checks undone (`recheck::forget`), checked
    /// again, and the passes after the bodies' check run (`crate::reanalyze`). `Err` says why a
    /// reload would take the cold check instead.
    pub fn reload_unchanged(
        &self,
        names: &[String],
        opts: crate::CheckOptions,
    ) -> Result<Box<crate::hir::Program>, String> {
        struct Shared<'a>(&'a Ctx<'static>);
        // SAFETY: the clone is made, used and dropped on sema's thread while this thread waits
        // for it; nothing else uses the kept context meanwhile (`&self`), so the `Rc`s it
        // shares with the clone are never touched from two threads at once.
        unsafe impl Sync for Shared<'_> {}
        impl Shared<'_> {
            // A method, so that the closure captures the wrapper, not the field.
            fn cx(&self) -> &Ctx<'static> {
                self.0
            }
        }
        let Some(before) = self.owned.before.as_deref() else {
            return Err("no context kept from before the passes".into());
        };
        let shared = Shared(before);
        crate::generating::on_sema_thread(|_| {
            let mut cx: Ctx<'static> = shared.cx().clone();
            let defs = (names.iter())
                .map(|name| find_fn(&cx, name))
                .collect::<Result<Vec<_>, String>>()?;
            let from = check_again(&mut cx, &defs, |_, _| Ok(()))?;
            finish(cx, opts, from).map(|(p, _)| p)
        })
    }

    /// The program after an edit of function bodies alone (dev-warm-front-end.md step 3 "C3"):
    /// `modules` is the edited program as the loader gives it (root module `root`), and `edited`
    /// names the functions whose bodies changed. From the context kept after the bodies' check,
    /// each one's first check is undone, it is given its body from `modules`, checked again, and
    /// the passes after the bodies' check run (`crate::reanalyze`). `Err` says why the edit needs
    /// the cold check: a module that moved, one the source rewrites change, a function that isn't
    /// a declaration or wasn't recorded, or what `reanalyze` refuses. With the program come its
    /// diagnostics, as the cold check gives them: the warnings of the code the edit left alone
    /// (an edited body that reports anything is refused).
    pub fn reload(
        &self,
        modules: &[SourceModule],
        root: usize,
        edited: &[(String, String)],
        opts: crate::CheckOptions,
    ) -> Result<(Box<crate::hir::Program>, velt_common::Diagnostics), String> {
        struct Shared<'a>(&'a Ctx<'static>, &'a [SourceModule]);
        // SAFETY: as in `reload_unchanged`; the edited modules are only read.
        unsafe impl Sync for Shared<'_> {}
        impl<'a> Shared<'a> {
            fn get(&self) -> (&Ctx<'static>, &'a [SourceModule]) {
                (self.0, self.1)
            }
        }
        let Some(before) = self.owned.before.as_deref() else {
            return Err("no context kept from before the passes".into());
        };
        let shared = Shared(before, modules);
        crate::generating::on_sema_thread(|_| {
            let (kept, modules) = shared.get();
            let mut cx: Ctx<'static> = kept.clone();
            // Every edited function is found in the old modules before any is swapped.
            let mut defs = vec![];
            for (module, name) in edited {
                let def = find_decl(&cx, module, name)?;
                let Some(crate::defs::FnSource::Decl(old)) = cx.fn_info(def).source else {
                    return Err(format!("`{name}` is not a declared function"));
                };
                let m = cx.fn_info(def).module;
                let at = decl_path(&cx.modules[m].ast, old).ok_or("not found in its module")?;
                let new = (modules.get(m))
                    .filter(|n| n.path == cx.modules[m].path)
                    .ok_or("the modules moved")?;
                let decl = decl_at(&new.ast, at).ok_or("not found in the edited module")?;
                defs.push((def, m, at, decl));
            }
            let mut edited_modules: Vec<usize> = defs.iter().map(|&(_, m, _, _)| m).collect();
            edited_modules.sort_unstable();
            edited_modules.dedup();
            // What the source rewrites and scans find of the program: the same as the kept
            // check's, but for the edited modules' own trees, which they leave as written.
            let (Some(kept_facts), Some(facts)) = (&cx.source_facts, probe(modules, root, opts))
            else {
                return Err("the source rewrites and scans were not kept".into());
            };
            if facts.rest != kept_facts.rest || facts.modules.len() != kept_facts.modules.len() {
                return Err("the edit changes what the source rewrites or scans find".into());
            }
            for (i, (now, then)) in facts.modules.iter().zip(&kept_facts.modules).enumerate() {
                let same = match edited_modules.contains(&i) {
                    true => modules.get(i).is_some_and(|m| *now == hash_of(&m.ast)),
                    false => now == then,
                };
                if !same {
                    let path = modules.get(i).map_or("", |m| m.path.as_str());
                    return Err(format!("the source rewrites change `{path}`"));
                }
            }
            for &m in &edited_modules {
                let at: Vec<_> = (defs.iter().filter(|d| d.1 == m)).map(|d| d.2).collect();
                if !others_stay(&cx.modules[m].ast, &modules[m].ast, &at) {
                    let path = &modules[m].path;
                    return Err(format!("the edit moves definitions after it in `{path}`"));
                }
            }
            for &m in &edited_modules {
                // The module is the edited one from here on: what a body's check reads of its
                // module (the names its function literals take, `ast::Module::fn_names`) is the
                // new text's. Its other definitions keep their old declarations, which are where
                // the new ones are (`others_stay`) and only their bodies set apart from them.
                // SAFETY: the clone, and every definition it makes from this module, lives only
                // until this closure returns; `modules` outlives the call.
                cx.modules[m] = unsafe { &*(&modules[m] as *const SourceModule) };
            }
            let ids: Vec<DefId> = defs.iter().map(|d| d.0).collect();
            let decls: Vec<(DefId, &'static velt_syntax::ast::FnDecl)> = (defs.iter())
                // SAFETY: as for the module above.
                .map(|&(d, _, _, decl)| (d, unsafe { &*(decl as *const _) }))
                .collect();
            let from = check_again(&mut cx, &ids, |cx, def| {
                if let Some(&(_, decl)) = decls.iter().find(|(d, _)| *d == def) {
                    cx.fn_info_mut(def).source = Some(crate::defs::FnSource::Decl(decl));
                }
                Ok(())
            })?;
            finish(cx, opts, from)
        })
    }

    /// The names of the program's own functions with a recorded body (the root module and the
    /// other modules that aren't std's or a package's), in definition order.
    pub fn own_functions(&self) -> Vec<String> {
        let cx = self.cx();
        let records = cx.body_records.as_ref();
        (0..cx.defs.len())
            .map(|i| crate::hir::DefId(i as u32))
            .filter(|d| records.is_some_and(|r| r.contains_key(d)))
            .filter(|d| {
                let m = cx.fn_info(*d).module;
                !cx.modules[m].is_std && !cx.modules[m].is_dependency
            })
            .map(|d| cx.fn_info(d).name.clone())
            .collect()
    }
}

/// The function named `name` among `cx`'s definitions.
fn find_fn(cx: &Ctx, name: &str) -> Result<crate::hir::DefId, String> {
    (0..cx.defs.len())
        .map(|i| crate::hir::DefId(i as u32))
        .find(|&d| {
            matches!(cx.info[d.0 as usize], crate::defs::DefInfo::Fn(_))
                && cx.fn_info(d).name == name
        })
        .ok_or_else(|| format!("no function `{name}`"))
}

/// Checks the bodies of `defs` again in `cx`: their first checks undone, all at once
/// (`recheck::forget_all`), each given its new declaration (`swap`) and checked again in the
/// order of the first checks, and what they now reach of std walked. `Err` when an edit changes
/// what other bodies' checks read of its function (`signature`), or makes std code live that the
/// kept check did not check or load. `Ok`: how many diagnostics the context had before the
/// bodies were checked again (the edited bodies' own went with their first checks).
fn check_again(
    cx: &mut Ctx<'static>,
    defs: &[DefId],
    mut swap: impl FnMut(&mut Ctx<'static>, DefId) -> Result<(), String>,
) -> Result<usize, String> {
    // The clone checks on this thread: how deep its checks nest is measured from here.
    cx.stack_base = crate::stack_address();
    let name = |cx: &Ctx, d: DefId| cx.fn_info(d).name.clone();
    let mut order: Vec<(usize, DefId)> = vec![];
    for &d in defs {
        let at = crate::body::recheck::recorded_at(cx, d);
        order.push((
            at.ok_or_else(|| format!("`{}` was not recorded", name(cx, d)))?,
            d,
        ));
    }
    order.sort_by_key(|&(at, d)| (at, d.0));
    let before: Vec<_> = defs.iter().map(|&d| (d, signature(cx, d))).collect();
    if let Err(d) = crate::body::recheck::forget_all(cx, defs) {
        return Err(format!("`{}` was not recorded", name(cx, d)));
    }
    let from = cx.diags.len();
    for &(_, d) in &order {
        swap(cx, d)?;
    }
    for &(_, d) in &order {
        crate::body::ensure_body(cx, d);
    }
    for (d, sig) in before {
        if signature(cx, d) != sig {
            return Err(format!(
                "the edit changes what `{}` returns or throws",
                name(cx, d)
            ));
        }
    }
    if crate::body::lazy_std::reach_again(cx, defs) {
        return Err("the edit reaches std code the kept check did not check or load".into());
    }
    Ok(from)
}

/// The passes after the bodies' check, on a context whose edited bodies were checked again;
/// any diagnostic past the first `from` refuses the reload. The program and its diagnostics.
fn finish(
    cx: Ctx<'static>,
    opts: crate::CheckOptions,
    from: usize,
) -> Result<(Box<crate::hir::Program>, velt_common::Diagnostics), String> {
    match crate::kept_reload::reanalyze(cx, opts, from) {
        Ok((program, diags)) => Ok((Box::new(program), diags)),
        Err(why) => Err(format!("the cold check: {why}")),
    }
}
