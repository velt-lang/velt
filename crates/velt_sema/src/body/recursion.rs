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

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind, RetSource};
use crate::hir::DefId;

#[derive(Default)]
pub(crate) struct RecState {
    /// Functions used before their inferred return type was known, by function.
    pending: HashMap<DefId, Pending>,
    /// Functions whose self-referring return type was reported.
    reported: Vec<DefId>,
    /// Checking a `return` expression of a body whose return type is inferred.
    pub in_return: bool,
    /// Bodies checked so far, in order, and whether their return type was inferred.
    completed: Vec<(DefId, bool)>,
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

/// What the checks since a body started added, to roll them back.
pub(crate) struct Mark {
    diags: usize,
    throw_checks: usize,
    fresh_checks: usize,
    jsx_adapters: usize,
    fn_values: usize,
    fn_defs: usize,
    reported: usize,
    completed: usize,
    ide: Option<[usize; 4]>,
}

impl Mark {
    /// The diagnostics and pending state as they are now.
    pub(crate) fn new(cx: &Ctx) -> Self {
        Mark {
            diags: cx.diags.len(),
            throw_checks: cx.throw_checks.len(),
            fresh_checks: cx.fresh_checks.len(),
            jsx_adapters: cx.jsx_adapters.len(),
            fn_values: cx.fn_values.len(),
            fn_defs: cx.fn_defs.len(),
            reported: cx.rec.reported.len(),
            completed: cx.rec.completed.len(),
            ide: cx
                .ide
                .as_ref()
                .map(|r| [r.refs.len(), r.types.len(), r.scopes.len(), r.params.len()]),
        }
    }

    /// Undoes what was checked since the mark: everything added is dropped (closures created
    /// meanwhile are left unreferenced), and the bodies checked meanwhile are checked again
    /// when next needed (keeping the return types they inferred unless those depended on a
    /// placeholder).
    fn rollback(&self, cx: &mut Ctx) {
        cx.diags.truncate(self.diags);
        cx.throw_checks.truncate(self.throw_checks);
        cx.fresh_checks.truncate(self.fresh_checks);
        cx.jsx_adapters.truncate(self.jsx_adapters);
        cx.fn_values.truncate(self.fn_values);
        cx.fn_defs.truncate(self.fn_defs);
        cx.rec.reported.truncate(self.reported);
        if let (Some(r), Some([refs, types, scopes, params])) = (&mut cx.ide, self.ide) {
            r.refs.truncate(refs);
            r.types.truncate(types);
            r.scopes.truncate(scopes);
            r.params.truncate(params);
        }
        for (g, inferred) in cx.rec.completed.split_off(self.completed) {
            cx.defs[g.0 as usize] = None;
            let error = cx.ty.has_error(cx.fn_info(g).ret);
            let f = cx.fn_info_mut(g);
            f.state = BodyState::Unchecked;
            if inferred && error {
                f.ret_source = RetSource::Body;
            }
        }
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
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, cx.display_in(p.ty, names)))
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
