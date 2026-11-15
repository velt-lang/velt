//! Units in sema (#2943 M1, docs/internals/design/build-architecture.md): with
//! `CheckOptions::units` (`VELT_UNITS` set for `velt check`),
//! std is collected and every std body checked as a unit of its own, before anything of the app;
//! then std's definitions are frozen ([`crate::ctx::DefTable`]) and the app is collected and its
//! bodies checked against them. A write to a std definition while the app is checked is a place
//! where the program still changes std, counted by location (`VELT_UNITS_FREEZE`) and printed at
//! the end of the check.
//!
//! This is the first step: the passes after the bodies (generated code, ownership, throws) still
//! run once over every definition, with the freeze lifted. They become per unit next, then the
//! round loop. With all of std checked, the driver loads every std file (`velt check`'s
//! `with_all_std`) and nothing is checked lazily (`body::lazy_std`).

use crate::ctx::Ctx;

/// Which part of a check by units a context checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// std, then the app, in one context (`VELT_UNITS=1`).
    Both,
    /// std alone, as a check of its own with its rounds (`VELT_UNITS=2`).
    Std,
    /// The app, from a copy of std's frozen unit (`VELT_UNITS=2`).
    App,
}

/// [`crate::check_definitions`] by units: std collected and checked, frozen, then the app.
pub(crate) fn check_definitions(cx: &mut Ctx) {
    let app: Vec<usize> = (0..cx.modules.len())
        .filter(|&m| !cx.modules[m].is_std && !cx.is_generated_module(m))
        .collect();
    if cx.units_stage != Stage::App {
        crate::timings::pass("units: std", || {
            crate::collect::collect_std(cx);
            // What a hint may name (`throws::hints`): the definitions collected, before the
            // bodies make closures.
            cx.collected_defs = cx.defs.len();
            crate::decorators::check(cx, 0);
            crate::body::expr::virtual_defaults::build_fill_bodies(cx, 0);
            crate::body::check_bodies(cx);
        });
    }
    if cx.units_stage == Stage::Std {
        // The passes after the bodies run for std alone (`crate::analyze`); it is frozen once
        // its rounds are done (`crate::rounds`).
        return;
    }
    if cx.units_stage == Stage::Both {
        cx.freeze_defs();
    }
    crate::timings::pass("units: app", || {
        crate::script_tdz::check(cx);
        let from = crate::collect::collect_app(cx, &app);
        cx.collected_defs = cx.defs.len();
        crate::decorators::check(cx, from.defs);
        crate::global_this::declare(cx);
        crate::body::check_added(cx, from);
        crate::body::finish_bodies(cx, from.defs);
    });
    if cx.units_stage == Stage::Both {
        cx.thaw_defs();
    }
}

/// EXPERIMENT: `VELT_UNITS=2`, std checked as a check of its own (its rounds and passes), then
/// the app's rounds from a copy of it.
pub(crate) fn stage2() -> bool {
    std::env::var_os("VELT_UNITS").is_some_and(|v| v == "2")
}
