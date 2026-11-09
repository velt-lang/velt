//! A check with error-set variables that reports something is checked again (`Ctx::checked_again`,
//! `throws::vars`): without any variable when it has errors, else with the solved sets written in,
//! and that check's diagnostics are the program's, since they show each variable's set. The check
//! again can be wrong where a check without variables is (#2602): one that reports an error the
//! check with variables did not report is not taken, and the first check's diagnostics stay the
//! program's ([`adds_errors`]).

use velt_common::Span;

use crate::ctx::Ctx;

/// Is `VELT_SITE_VARS_FIRST` set (not empty): the diagnostics of the check with variables as it
/// reports them, without checking again (what the identity test compares a check again with)?
pub(crate) fn first_check_only() -> bool {
    std::env::var_os("VELT_SITE_VARS_FIRST").is_some_and(|v| !v.is_empty())
}

/// Does `again`, the check of the program again, report an error where `first` reported none?
pub(crate) fn adds_errors(first: &Ctx, again: &Ctx) -> bool {
    let at = |cx: &Ctx| -> Vec<Option<Span>> {
        (cx.diags.iter())
            .filter(|d| d.is_error())
            .map(|d| d.labels.first().map(|l| l.span))
            .collect()
    };
    let before = at(first);
    at(again).iter().any(|s| !before.contains(s))
}
