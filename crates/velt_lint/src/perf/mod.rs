//! The `perf/…` rules on the syntax tree and the checker's types ([`crate::Kind::Pattern`]).
//! Each one finds a TypeScript habit that costs speed in Velt and, where the rewrite is
//! mechanical, offers it, saying whether the rewrite is still TypeScript.

mod class_data;
mod foreach;
mod record;

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, FileId, SourceMap, Span};
use velt_sema::{ide, SourceModule};
use vpm::manifest::LintLevel;

use crate::{Finding, Levels, Profile, Profiles};

/// What a rule sees: the program and the module it lints.
pub(crate) struct Cx<'a> {
    pub modules: &'a [SourceModule],
    pub sm: &'a SourceMap,
    pub analysis: &'a ide::Analysis,
    pub module: &'a SourceModule,
}

impl Cx<'_> {
    /// The source text of `span` (in any loaded file).
    pub fn text(&self, span: Span) -> &str {
        let src = &self.sm.get(span.file).src;
        src.get(span.lo as usize..span.hi as usize).unwrap_or("")
    }

    /// The type of the expression with exactly this span.
    pub fn view(&self, span: Span) -> Option<ide::TypeView> {
        self.analysis.type_of(span).map(|t| self.analysis.view(&t))
    }
}

/// The profile of every loaded file: the standard library's and dependencies' are
/// [`Profile::Dependency`], the others' as `profiles` says.
pub fn file_profiles(
    modules: &[SourceModule],
    sm: &SourceMap,
    profiles: &Profiles,
) -> HashMap<FileId, Profile> {
    modules
        .iter()
        .map(|m| {
            let profile = if m.is_std || m.is_dependency {
                Profile::Dependency
            } else {
                profiles.of(&velt_tscompat::canonical(&sm.get(m.file).path))
            };
            (m.file, profile)
        })
        .collect()
}

/// The pattern rules' findings in the modules `lint` selects that have no error in
/// `diagnostics`, as diagnostics at the package's levels (suppressions not applied yet:
/// [`crate::finish`] does that for every family at once).
pub fn lint(
    modules: &[SourceModule],
    sm: &SourceMap,
    diagnostics: &[Diagnostic],
    analysis: &ide::Analysis,
    profiles: &Profiles,
    levels: &Levels,
    lint: &dyn Fn(FileId) -> bool,
) -> Vec<Diagnostic> {
    let profile_of = file_profiles(modules, sm, profiles);
    let failed: HashSet<FileId> = diagnostics
        .iter()
        .filter(|d| d.is_error())
        .filter_map(|d| d.labels.first().map(|l| l.span.file))
        .collect();
    let of_span = |s: Span| {
        profile_of
            .get(&s.file)
            .copied()
            .unwrap_or(Profile::Dependency)
    };
    let on = |code: &str| levels.perf(code) != LintLevel::Off;
    let mut out = vec![];
    for module in modules {
        let profile = of_span(Span::new(module.file, 0, 0));
        if profile == Profile::Dependency || !lint(module.file) || failed.contains(&module.file) {
            continue;
        }
        let cx = Cx {
            modules,
            sm,
            analysis,
            module,
        };
        let mut found: Vec<Finding> = vec![];
        if on(foreach::CODE) {
            found.extend(foreach::find(&cx));
        }
        if on(record::CODE) {
            found.extend(record::find(&cx));
        }
        if on(class_data::CODE) {
            found.extend(class_data::find(&cx));
        }
        found.sort_by_key(|f| (f.span.lo, f.span.hi));
        out.extend(
            found
                .into_iter()
                .filter_map(|f| crate::report(f, profile, &of_span, levels)),
        );
    }
    out
}

/// Whether `e` (or anything inside it, nested functions excluded) satisfies `f`.
pub(crate) fn any_expr(
    e: &velt_syntax::ast::Expr,
    f: &dyn Fn(&velt_syntax::ast::Expr) -> bool,
) -> bool {
    struct Find<'f> {
        f: &'f dyn Fn(&velt_syntax::ast::Expr) -> bool,
        hit: bool,
    }
    impl<'a> velt_syntax::visit::Visit<'a> for Find<'_> {
        fn expr(&mut self, e: &'a velt_syntax::ast::Expr) {
            self.hit |= (self.f)(e);
        }
    }
    let mut find = Find { f, hit: false };
    velt_syntax::visit::walk_expr(e, &mut find);
    find.hit
}
