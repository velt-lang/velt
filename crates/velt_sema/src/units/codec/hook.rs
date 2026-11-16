//! Tests' hook into a check by units (`crate::units`): while a [`Hook`] is installed ([`with`]),
//! a check with `CheckOptions::units` checks std as a stage of its own (as `VELT_UNITS=2` does),
//! shows std's frozen unit to the hook, and may start the app's stage from that unit encoded and
//! decoded ([`encode_unit`] / [`decode_unit`]) instead of from the unit itself. Only checks with
//! `units` set consult it, and only these tests set it; the tests that install a hook take turns.

use std::sync::Mutex;

use super::{decode_unit, encode_unit, Arena};
use crate::ctx::Ctx;

/// Called with std's frozen unit as std's stage ends.
pub(crate) type Inspect = Box<dyn FnMut(&Ctx<'_>) + Send>;

pub(crate) struct Hook {
    pub(crate) inspect: Option<Inspect>,
    /// The app's stage starts from std's unit encoded and decoded.
    pub(crate) decoded: bool,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

/// Taken by each test that installs a hook, for its whole run.
static TURN: Mutex<()> = Mutex::new(());

/// `f`, with `hook` installed.
pub(crate) fn with<T>(hook: Hook, f: impl FnOnce() -> T) -> T {
    let _turn = TURN.lock().unwrap_or_else(|p| p.into_inner());
    struct Uninstall;
    impl Drop for Uninstall {
        fn drop(&mut self) {
            *HOOK.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
    *HOOK.lock().unwrap_or_else(|p| p.into_inner()) = Some(hook);
    let _uninstall = Uninstall;
    f()
}

/// Is a hook installed (`units::stage2`)?
pub(crate) fn installed() -> bool {
    HOOK.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

/// std's unit as the app's stage takes it: shown to the hook, and encoded and decoded if the hook
/// asks. The arena of a decoded unit is leaked (a test's).
pub(crate) fn std_unit<'m>(cx: Ctx<'m>) -> Ctx<'m> {
    let mut hook = HOOK.lock().unwrap_or_else(|p| p.into_inner());
    let Some(hook) = hook.as_mut() else {
        return cx;
    };
    if let Some(inspect) = &mut hook.inspect {
        inspect(&cx);
    }
    if !hook.decoded {
        return cx;
    }
    let bytes = std::rc::Rc::new(encode_unit(&cx));
    let arena: &'static Arena = Box::leak(Box::default());
    // SAFETY: the arena is leaked, so it outlives the context.
    let mut out =
        unsafe { decode_unit(&bytes, arena, cx.modules.clone()) }.expect("ICE: std's unit decodes");
    // What the check set when it made std's context, which an app's round keeps (`new_cx`).
    out.units = cx.units;
    out.units_stage = cx.units_stage;
    out.stack_budget = cx.stack_budget;
    out
}
