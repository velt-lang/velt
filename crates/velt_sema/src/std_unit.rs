//! std's unit kept between checks of one process (#2943, dev-warm-front-end.md step 3a): a
//! long-lived caller (the language server, the dev host) checks one program after another, and
//! std's frozen unit (`crate::units`) is the same for each while std's sources and what the check
//! hands std's stage are. A check by units with a key (`CheckOptions::std_key`) takes the unit
//! from here when its key matches, and keeps the unit it made otherwise.
//!
//! What the unit borrows it owns: std's modules, the source rewrites' copies the unit was checked
//! over ([`StdUnit::modules`]). std's stage borrows nothing else of a check: it generates no code
//! (`generated_phase`), takes no generator, loads nothing and its facts per module are its own
//! (`units::remap_module_facts`).
//!
//! Safety, in one place. The unit's context is erased to `'static`: everything it borrows is the
//! unit's own and stays at the same heap address (a boxed slice's contents do not move with the
//! box), and the context is dropped first ([`StdUnit`]'s `Drop`). The cache crosses threads (each
//! check runs on a thread of its own, `generating::on_sema_thread`) though its `Rc`s are not
//! atomic: a check holds the cache's lock from before it copies the unit until after every value
//! made from the copy is dropped (`Held` outlives them in `check_timed`), so no two threads touch
//! those counts at once, and what a check returns holds no `Rc` (it is `Send`).

use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::sync::{Mutex, MutexGuard};

use crate::ctx::Ctx;
use crate::SourceModule;

/// std's frozen unit and the modules it borrows.
pub(crate) struct StdUnit {
    ctx: ManuallyDrop<Box<Ctx<'static>>>,
    /// Held for the context's borrows; dropped after it.
    #[allow(dead_code)]
    modules: Box<[SourceModule]>,
    key: u64,
}

impl Drop for StdUnit {
    fn drop(&mut self) {
        // SAFETY: dropped once, here, before `modules`, which it borrows (module docs).
        unsafe { ManuallyDrop::drop(&mut self.ctx) }
    }
}

// SAFETY: module docs (the lock serializes every use of the context's `Rc`s).
unsafe impl Send for StdUnit {}

static CACHE: Mutex<Option<StdUnit>> = Mutex::new(None);

/// The cache, locked for one check; `None` when another check holds it (that one runs without).
pub(crate) type Held = Option<MutexGuard<'static, Option<StdUnit>>>;

/// Lock the cache for a check with a key.
pub(crate) fn hold(key: Option<u64>) -> Held {
    key?;
    match CACHE.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            let mut g = p.into_inner();
            *g = None;
            Some(g)
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// A copy of the unit kept for `key`, if the cache holds it.
pub(crate) fn take<'m>(held: &Held, key: Option<u64>) -> Option<Ctx<'m>> {
    let unit = held.as_ref()?.as_ref()?;
    (Some(unit.key) == key).then(|| Ctx::clone(&unit.ctx))
}

/// Keep `cx`, std's frozen unit, checked over `modules` (which own what it borrows), for `key`.
///
/// # Safety
///
/// `cx` must borrow nothing but `modules`' contents: std's stage over [`copies`] of std's
/// modules, with nothing of the check's installed in it (module docs).
pub(crate) unsafe fn keep(held: &mut Held, key: u64, cx: Ctx<'_>, modules: Box<[SourceModule]>) {
    let Some(slot) = held.as_mut() else {
        return;
    };
    // SAFETY: the caller's contract; `StdUnit` owns `modules` and drops the context first.
    let ctx = unsafe { std::mem::transmute::<Ctx<'_>, Ctx<'static>>(cx) };
    **slot = Some(StdUnit {
        ctx: ManuallyDrop::new(Box::new(ctx)),
        modules,
        key,
    });
}

/// std's modules of `modules`, copied for a unit to own.
pub(crate) fn copies(modules: &[SourceModule]) -> Box<[SourceModule]> {
    (modules.iter().filter(|m| crate::units::in_first_unit(m)))
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            is_dependency: m.is_dependency,
            file: m.file,
            ast: m.ast.clone(),
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect()
}

/// The key of std's unit for a check: the caller's key for std's sources, with what the check
/// hands std's stage (`parts`; spans only those in std's `files`, as an edit of the program moves
/// its own).
pub(crate) fn key(std_key: u64, parts: &[&dyn std::fmt::Debug]) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    std_key.hash(&mut h);
    for p in parts {
        format!("{p:?}").hash(&mut h);
    }
    h.finish()
}

/// The spans of `spans` in std's `files`, sorted (a set's order is the hasher's).
pub(crate) fn std_spans<'a>(
    spans: impl IntoIterator<Item = &'a velt_common::Span>,
    files: &rustc_hash::FxHashSet<velt_common::FileId>,
) -> Vec<(u32, u32, u32)> {
    let mut v: Vec<(u32, u32, u32)> = (spans.into_iter())
        .filter(|s| files.contains(&s.file))
        .map(|s| (s.file.0, s.lo, s.hi))
        .collect();
    v.sort_unstable();
    v
}
