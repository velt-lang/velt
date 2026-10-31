//! The entry point of a check that may use generated functions
//! (docs/internals/design/source-generators.md "Rounds"): [`check_generating`] returns the
//! program, or the instances that have no generated code yet for the driver to generate. Also
//! the thread every check runs on ([`on_sema_thread`]).

use velt_common::{Diagnostic, Diagnostics};

use crate::{check_on_current_thread, generated, hir, CheckOptions, SourceModule};
use crate::{FALLBACK_STACK_BUDGET, SEMA_STACK_BUDGET, SEMA_STACK_BYTES};

/// What [`check_generating`] gives: the program, or the instances of generated functions that
/// have no generated code yet (docs/internals/design/source-generators.md "Rounds").
#[derive(Debug)]
pub enum Checked {
    Program(Box<hir::Program>),
    Demand(generated::Demand),
    /// Code generated during the check needs a check of its own (`generated::phase`
    /// `needs_rewrite`): the driver checks again with everything generated so far.
    Again,
}

/// [`crate::check_with`] of a program that may use generated functions, with the code generated
/// for them so far (`gen`). With `generate`, the check asks the driver for the code of the
/// instances that have none and adds it to itself (docs/internals/design/source-generators.md
/// §6); without, or when that code needs a check of its own, it returns a [`Checked::Demand`]
/// (or [`Checked::Again`]) and the driver checks again with the code added.
pub fn check_generating(
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    gen: &generated::GenInput,
    sources: &[SourceModule],
    generate: Option<&generated::GenerateFn>,
) -> (Option<Checked>, Diagnostics) {
    let args = generated::GenArgs {
        input: gen,
        sources,
        generate,
    };
    on_sema_thread(|budget| check_on_current_thread(modules, root, opts, &[], Some(args), budget))
}

/// Give a new `Ctx` of a check the code generated so far (`gen`), and the driver's generator:
/// the generated modules given apart from the program's and those the generator made during
/// an earlier `Ctx` of the check are added to it (`Ctx::add_module`).
pub(crate) fn install<'m>(
    cx: &mut crate::ctx::Ctx<'m>,
    gen: Option<generated::GenArgs<'m>>,
    generator: Option<&'m generated::Generator<'m>>,
) {
    let Some(gen) = gen else {
        return;
    };
    let mut input = gen.input.clone();
    for m in gen.sources {
        let declaring = (input.modules.iter())
            .find(|gm| gm.path == m.path)
            .and_then(|gm| cx.modules.iter().position(|d| d.path == gm.declaring));
        if let Some(d) = declaring {
            cx.add_module(m, d);
        }
    }
    if let Some(generator) = generator {
        let state = generator.state.borrow();
        input.extend(state.input.clone());
        let declaring = state.filled.clone();
        drop(state);
        for (m, d) in generator.kept().zip(declaring) {
            cx.add_module(m, d);
        }
    }
    cx.gen.input = Some(std::rc::Rc::new(input));
    cx.gen.generate = generator;
}

impl<'m> crate::ctx::Ctx<'m> {
    /// Add module `m`, a generated module made during the check (`generated::phase`), with the
    /// import targets of the module it was generated for (`declaring`): its index.
    pub(crate) fn add_module(&mut self, m: &'m SourceModule, declaring: usize) -> usize {
        let i = self.modules.len();
        self.modules.push(m);
        self.scopes.push(crate::ctx::ModuleScope {
            is_std: m.is_std,
            is_library: m.is_std || m.is_dependency,
            ..Default::default()
        });
        let targets = self.import_targets[declaring].clone();
        self.import_targets.push(targets);
        self.lazy.add_module();
        i
    }
}

/// What the check gives when the driver's generator stopped it: nothing when the generator
/// failed (it reported why), [`Checked::Again`] when it made code this check could not add.
pub(crate) fn stopped(generator: Option<&generated::Generator>) -> Option<Option<Checked>> {
    let state = generator?.state.borrow();
    match (state.failed, state.restart) {
        (true, _) => Some(None),
        (false, true) => Some(Some(Checked::Again)),
        _ => None,
    }
}

/// The program of a check whose caller runs no generators: demand is an error at each call.
pub(crate) fn program_or_demand_error(
    out: Option<Checked>,
    mut diags: Diagnostics,
) -> (Option<hir::Program>, Diagnostics) {
    match out {
        Some(Checked::Program(p)) => (Some(*p), diags),
        Some(Checked::Again) => (None, diags),
        Some(Checked::Demand(d)) => {
            for r in &d.requests {
                diags.push(Diagnostic::error(
                    format!(
                        "`{}` is a generated function, and this command does not run generators",
                        d.functions[r.function].function
                    ),
                    r.at,
                ));
            }
            (None, diags)
        }
        None => (None, diags),
    }
}

/// `check(budget)` on a thread with sema's stack ([`crate::SEMA_STACK_BUDGET`]), or on this thread
/// with [`crate::FALLBACK_STACK_BUDGET`] when no thread can be made.
pub(crate) fn on_sema_thread<T: Send>(check: impl Fn(usize) -> T + Sync) -> T {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-sema".into())
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || check(SEMA_STACK_BUDGET));
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => check(FALLBACK_STACK_BUDGET),
        }
    })
}
