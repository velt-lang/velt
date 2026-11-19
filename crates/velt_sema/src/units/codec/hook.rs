//! Tests' hook into a check by units (`crate::units`): while a [`Hook`] is installed ([`with`]),
//! a check with `CheckOptions::units` shows std's frozen unit to the hook as the app's stage
//! starts. Only checks with `units` set consult it, and only these tests set it; the tests that
//! install a hook take turns.

use std::sync::Mutex;

use crate::ctx::Ctx;

/// Called with std's frozen unit as std's stage ends.
pub(crate) type Inspect = Box<dyn FnMut(&Ctx<'_>) + Send>;

pub(crate) struct Hook {
    pub(crate) inspect: Option<Inspect>,
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

/// Show std's frozen unit `cx` to the hook installed, if any.
pub(crate) fn std_unit(cx: &Ctx<'_>) {
    let mut hook = HOOK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(inspect) = hook.as_mut().and_then(|h| h.inspect.as_mut()) {
        inspect(cx);
    }
}
