//! The rounds of a check: the whole check runs again while what one round found changes what
//! the program is checked with (`lib.rs` `check_timed`): grown error sets of function types
//! (`throws::fn_types`), new families of union views (`union_views`), the sharing retry, std
//! bodies known too late (`body::lazy_std`), and doubted error-set variables (`FnSites`).
//! Factored out so a check by units (`crate::units`) can run the rounds of one unit.

use std::cell::RefCell;

use crate::body::lazy_std::Assumed;
use crate::ctx::Ctx;
use crate::hir::DefId;
use crate::{
    body, error_count, generated, generating, retry_sharing, script_locals, throws, timings,
    union_views, Checked, FN_SITE_ROUNDS, VIEW_ROUNDS,
};

/// How a round starts: its name, whether borrows are held, the error-set seeds, whether it is
/// the last, and the union-view families assumed.
pub(crate) type NewCx<'a, 'm> = dyn Fn(
        &'static str,
        bool,
        &throws::FnTypeSeeds,
        bool,
        &union_views::Families,
    ) -> (Ctx<'m>, Option<DefId>)
    + 'a;

/// The rounds of a check, each started by `new_cx`, until none finds more: the last round's
/// context, entry and bound generated code, or the check's result when it stops early (errors,
/// a generator that stopped, a demand for generated code).
pub(crate) fn run<'m>(
    new_cx: &NewCx<'_, 'm>,
    doubt: &dyn Fn(&Ctx) -> bool,
    assumed: &RefCell<Assumed>,
    recovered: bool,
    generator: Option<&generated::Generator>,
) -> Result<(Ctx<'m>, Option<DefId>, generated::bind::Bound), Box<script_locals::Checked>> {
    // Function types written without `throws` throw what the closures stored into them throw
    // (`throws::fn_types`): check again while those sets grow.
    let mut seeds = throws::FnTypeSeeds::default();
    // Unions of interfaces made views of (`union_views`): their uses are checked again with
    // arms for the view.
    let mut views = union_views::Families::default();
    let mut view_rounds = 0;
    let (mut cx, mut entry) = new_cx("round: first", true, &seeds, false, &views);
    let (mut first, mut kept_first) = (None, false);
    // A check with sharing may grow the sets too: the checks after it share as well.
    let (mut rounds, mut held) = (0, true);
    let generated = 'check: loop {
        loop {
            // Views of new families (`union_views`) are checked again together with the growth of
            // the throws sets, in one round.
            let grown_views = cx.union_views_grown().filter(|_| view_rounds < VIEW_ROUNDS);
            let grown_seeds = cx.fn_sites.grown();
            if grown_views.is_some() || grown_seeds.is_some() {
                if let Some(g) = grown_views {
                    view_rounds += 1;
                    views = g;
                }
                if let Some(g) = grown_seeds {
                    rounds += 1;
                    seeds = g;
                }
                let last = rounds == FN_SITE_ROUNDS;
                (cx, entry) = new_cx("round: grown throws sets", held, &seeds, last, &views);
            } else if held && retry_sharing(&cx) {
                held = false;
                let last = rounds == FN_SITE_ROUNDS;
                (cx, entry) = new_cx("round: sharing retry", false, &seeds, last, &views);
            } else if error_count(&cx) == 0
                && body::lazy_std::too_late(&mut cx, &mut assumed.borrow_mut())
            {
                let last = rounds == FN_SITE_ROUNDS;
                (cx, entry) = new_cx("round: known too late", held, &seeds, last, &views);
            } else {
                break;
            }
        }
        union_views::finish(&mut cx);
        cx.upcast_writes();
        // What these passes and the rounds reported, as after the first check.
        let doubted = "round: doubted throws variables";
        if let Some((f, e)) = first.take().filter(|(f, _)| throws::adds_errors(f, &cx)) {
            (cx, entry, kept_first) = (f, e, true);
        } else if let Some(s) = cx.checked_again(doubt(&cx) && !kept_first) {
            seeds = s;
            let (c, e) = new_cx(doubted, held, &seeds, rounds == FN_SITE_ROUNDS, &views);
            first = Some((
                std::mem::replace(&mut cx, c),
                std::mem::replace(&mut entry, e),
            ));
            continue 'check;
        }
        if recovered || cx.diags.iter().any(|d| d.is_error()) {
            let refused = script_locals::refused(&cx, cx.root);
            return Err(Box::new(((None, cx.diags), refused)));
        }
        if let Some(out) = generating::stopped(generator) {
            return Err(Box::new(((out, cx.diags), Default::default())));
        }
        // std's unit binds no instance: the app's stage does (`crate::units`).
        let generated = match cx.units_stage == crate::units::Stage::Std {
            true => Ok(Default::default()),
            false => timings::pass("generated::instances", || generated::instances(&mut cx)),
        };
        let generated = match generated {
            Ok(bound) => bound,
            Err(demand) => {
                return Err(Box::new((
                    (Some(Checked::Demand(demand)), cx.diags),
                    Default::default(),
                )))
            }
        };
        if let Some(s) = cx.checked_again(doubt(&cx) && !kept_first) {
            seeds = s;
            (cx, entry) = new_cx(doubted, held, &seeds, rounds == FN_SITE_ROUNDS, &views);
            continue 'check;
        }
        if cx.diags.iter().any(|d| d.is_error()) {
            return Err(Box::new(((None, cx.diags), Default::default())));
        }
        break generated;
    };
    Ok((cx, entry, generated))
}
