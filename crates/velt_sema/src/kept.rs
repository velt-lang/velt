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
    /// Set on sema's thread for one check: keep the check of exactly these modules.
    static REQUEST: Cell<Option<Place>> = const { Cell::new(None) };
    /// What the check kept, for [`take`].
    static OFFER: RefCell<Option<PendingKept>> = const { RefCell::new(None) };
}

/// Ask the check about to run on this thread to keep itself, if it checks `modules`.
pub(crate) fn request(modules: &[SourceModule]) {
    REQUEST.with(|r| r.set(Some(place(modules))));
    OFFER.with(|o| o.borrow_mut().take());
}

/// What the check kept, and the request cleared.
pub(crate) fn take() -> Option<PendingKept> {
    REQUEST.with(|r| r.set(None));
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
    checked: *const SourceModule,
    stages: Vec<Stage>,
    loads: Option<usize>,
) {
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
    let pending = PendingKept {
        owned: Owned {
            ctx: ManuallyDrop::new(Box::new(ctx)),
            stage,
            input: None,
            sources: None,
            loads: None,
        },
        input: place(input),
        sources: place(sources),
        loads,
    };
    OFFER.with(|o| *o.borrow_mut() = Some(pending));
}

/// A kept check's clone and what it borrows, dropped in that order.
struct Owned {
    ctx: ManuallyDrop<Box<Ctx<'static>>>,
    /// Held for the modules the context borrows from it.
    stage: Option<Box<dyn Any>>,
    input: Option<Vec<SourceModule>>,
    sources: Option<Vec<SourceModule>>,
    loads: Option<StdLoads>,
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: dropped once, here, before the fields it borrows (dropped after this).
        unsafe { ManuallyDrop::drop(&mut self.ctx) }
    }
}

/// A kept check that still borrows the driver's modules and std loads: [`PendingKept::adopt`]
/// takes them over.
pub struct PendingKept {
    owned: Owned,
    input: Place,
    sources: Place,
    loads: Option<usize>,
}

// SAFETY: module docs (the clone's `Rc`s were shared only with a context dropped before the
// check's thread was joined).
unsafe impl Send for PendingKept {}

impl PendingKept {
    /// The kept check, owning the modules the check was given (`input`, the driver's parsed
    /// modules or its copy of them), the generated `sources` given with them, and the std
    /// `loads`, if they are the ones the check borrowed (the same heap addresses); otherwise they
    /// come back.
    pub fn adopt(
        mut self,
        input: Vec<SourceModule>,
        sources: Vec<SourceModule>,
        loads: Option<StdLoads>,
    ) -> Result<KeptCheck, Refused> {
        let same_loads = match (self.loads, &loads) {
            (None, _) => true,
            (Some(id), Some(l)) => l.id() == id,
            (Some(_), None) => false,
        };
        if place(&input) != self.input || place(&sources) != self.sources || !same_loads {
            return Err(Refused {
                input,
                sources,
                loads,
            });
        }
        // A context that checked the source rewrites' copy borrows nothing of `input` (each
        // rewrite copies every module): only the copy is kept, which halves what is kept.
        self.owned.input = self.owned.stage.is_none().then_some(input);
        self.owned.sources = Some(sources);
        self.owned.loads = loads;
        Ok(KeptCheck { owned: self.owned })
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

    /// Does the context borrow the source rewrites' copy of the modules (which the kept check
    /// owns), rather than the driver's modules themselves?
    pub fn owns_rewritten_modules(&self) -> bool {
        self.owned.stage.is_some()
    }

    /// The number of the context's definitions.
    pub fn definitions(&self) -> usize {
        self.cx().defs.len()
    }

    /// The number of top-level items of each module, read through the context's borrows.
    pub fn items(&self) -> usize {
        self.cx().modules.iter().map(|m| m.ast.items.len()).sum()
    }
}
