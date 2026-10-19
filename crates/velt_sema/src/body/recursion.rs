//! Recursion in functions whose return types are inferred, with TypeScript's rule: only a use
//! of the function inside its own `return` expressions (directly, or through other functions
//! whose return expressions use it) needs a return type annotation.
//!
//! A use of a function whose body is being checked (`ensure_body`) cannot know the function's
//! type yet: it gets the error type (a placeholder, which silences the errors that follow from
//! it) and is recorded. When the body is done, its return type is inferred from its `return`s.
//! If that type contains the error type, the returns depended on the function itself: that is
//! reported, and the body stays as checked. Otherwise the type is known: the work done since
//! the body started (diagnostics, IDE records, the bodies checked meanwhile) is rolled back and
//! the body is checked again against the known type. So a function body is checked at most
//! twice for each recursive function being inferred around it (normally once), and only
//! programs with such recursion pay for the second pass.

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};

use super::recheck::Mark;
use crate::ctx::Ctx;
use crate::defs::{FnKind, FnSource};
use crate::hir::DefId;
use crate::written_types::written_param;

#[derive(Default)]
pub(crate) struct RecState {
    /// Functions used before their inferred return type was known, by function.
    pending: HashMap<DefId, Pending>,
    /// Functions whose self-referring return type was reported.
    pub(super) reported: Vec<DefId>,
    /// Checking a `return` expression of a body whose return type is inferred.
    pub in_return: bool,
    /// Bodies checked so far, in order, and whether their return type was inferred.
    pub(super) completed: Vec<(DefId, bool)>,
}

/// The first use of a function before its return type is known (preferring one in a `return`
/// expression): where, and the bodies being checked from the function to the use.
#[derive(Clone)]
pub(crate) struct Pending {
    at: Span,
    in_return: bool,
    chain: Vec<DefId>,
}

/// `d`'s return type is needed at `at` while its body, which decides it, is being checked.
pub(crate) fn placeholder(cx: &mut Ctx, d: DefId, at: Span) {
    let start = cx.checking.iter().position(|x| *x == d).unwrap_or(0);
    let p = Pending {
        at,
        in_return: cx.rec.in_return,
        chain: cx.checking[start..].to_vec(),
    };
    let e = cx.rec.pending.entry(d).or_insert_with(|| p.clone());
    if p.in_return && !e.in_return {
        *e = p;
    }
}

/// A body was checked (`inferred`: its return type was inferred).
pub(crate) fn completed(cx: &mut Ctx, d: DefId, inferred: bool) {
    cx.rec.completed.push((d, inferred));
}

/// After the first check of `d`'s body: whether it must be checked again (its return type,
/// now known, was needed before; everything since `mark` has been rolled back).
pub(crate) fn needs_second_pass(cx: &mut Ctx, d: DefId, mark: &Mark) -> bool {
    let Some(p) = cx.rec.pending.remove(&d) else {
        return false;
    };
    if cx.ty.has_error(cx.fn_info(d).ret) {
        report_cycle(cx, d, &p);
        return false;
    }
    // A function being inferred around this one also waits: its second pass checks this
    // body again anyway.
    let outer_waits = cx
        .checking
        .iter()
        .any(|o| *o != d && cx.rec.pending.contains_key(o));
    if outer_waits {
        return false;
    }
    mark.rollback(cx);
    true
}

/// `d`'s return type depends on itself (`p`: where it is used).
fn report_cycle(cx: &mut Ctx, d: DefId, p: &Pending) {
    if cx.rec.reported.contains(&d) {
        return;
    }
    cx.rec.reported.push(d);
    let mut chain: Vec<String> = p
        .chain
        .iter()
        .map(|x| format!("`{}`", short_name(cx, *x)))
        .collect();
    let f = cx.fn_info(d);
    let name = short_name(cx, d);
    let what = if f.kind == FnKind::Free {
        "function"
    } else {
        "method"
    };
    let why = if chain.len() <= 1 {
        format!(
            "its return type is inferred from its `return` expressions, which use `{name}` itself"
        )
    } else {
        chain.push(format!("`{name}`"));
        format!(
            "its return type is inferred from its `return` expressions, which use it again: {}",
            chain.join(" → ")
        )
    };
    let fix = format!("write the return type: `{}`", annotated_signature(cx, d));
    let span = f.name_span;
    cx.error(
        Diagnostic::error(
            format!("{what} `{name}` needs a return type annotation"),
            span,
        )
        .with_label(p.at, "used here before its return type is known")
        .with_note(why)
        .with_note(fix),
    );
}

/// `d`'s declaration with a placeholder return type, as the user writes it: a function, a
/// generic arrow constant or a method, with its own type parameters and its parameters.
fn annotated_signature(cx: &Ctx, d: DefId) -> String {
    let f = cx.fn_info(d);
    let names = &f.generics.names;
    let owner = f
        .owner
        .and_then(|o| cx.adt(o))
        .map_or(0, |a| a.generics.len());
    let own = &names[owner.min(names.len())..];
    let generics = if own.is_empty() {
        String::new()
    } else {
        format!("<{}>", own.join(", "))
    };
    // As written where the declaration is at hand (`n: number`, not the resolved `f64`).
    let written = match f.source {
        Some(FnSource::Decl(decl)) => Some(&decl.sig),
        Some(FnSource::Default(sig, _)) => Some(sig),
        None => None,
    }
    .filter(|sig| sig.params.len() == f.params.len());
    let params: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| match written {
            Some(sig) => {
                let wp = &sig.params[i];
                let q = if wp.optional { "?" } else { "" };
                let rest = if wp.rest { "..." } else { "" };
                format!("{rest}{}{q}: {}", p.name, written_param(wp))
            }
            None => format!("{}: {}", p.name, cx.display_in(p.ty, names)),
        })
        .collect();
    let params = params.join(", ");
    let name = short_name(cx, d);
    let simple = name.rsplit('.').next().unwrap_or(&name);
    if cx.generic_arrow_all.contains(&f.name_span) {
        format!("const {simple} = {generics}({params}): … =>")
    } else if f.kind == FnKind::Free {
        format!("function {simple}{generics}({params}): …")
    } else {
        format!("{simple}{generics}({params}): …")
    }
}

/// Function `d`'s name without its module path (`Countdown.count`).
pub(crate) fn short_name(cx: &Ctx, d: DefId) -> String {
    let name = &cx.fn_info(d).name;
    name.rsplit("::").next().unwrap_or(name).to_string()
}
