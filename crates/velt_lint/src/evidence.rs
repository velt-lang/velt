//! The `perf/…` rules on what the optimizer could not do ([`crate::Kind::Evidence`]), run by
//! `velt check --perf` on the optimized program. The caller (veltc) turns the optimizer's
//! reports into the plain records here, so this crate needs no optimizer; each finding is placed
//! at the source the record names, in a file whose profile allows a fix there.

use std::collections::HashSet;
use std::path::PathBuf;

use velt_common::{Applicability, Diagnostic, Fix, SourceMap, Span};
use velt_sema::ide::{self, DefKind};
use velt_sema::SourceModule;
use velt_syntax::ast;
use velt_syntax::visit::{walk_module, Visit};

use crate::perf::file_profiles;
use crate::{Finding, Levels, Profile, ProfiledFix, Profiles};

pub const NUMBER_DOUBLE: &str = "perf/number-double";

/// A `number` variable in a loop that the optimizer keeps as a double (`velt build --report
/// numbers`).
#[derive(Clone, Debug)]
pub struct NumberDouble {
    /// The file as the optimized program names it.
    pub file: PathBuf,
    /// Where it is first assigned in the loop (1-based; the column counts bytes).
    pub line: u32,
    pub col: u32,
    pub name: String,
    /// Why, in words.
    pub reason: String,
    /// Every value is a whole number from integer operations, but nothing bounds it: an integer
    /// type holds it exactly.
    pub unbounded: bool,
}

/// The findings for `items` at the package's levels. Records in a dependency's file are left
/// out unless `deps`, and then they are notes without fixes, for its author.
#[allow(clippy::too_many_arguments)]
pub fn number_double(
    items: &[NumberDouble],
    modules: &[SourceModule],
    sm: &SourceMap,
    analysis: &ide::Analysis,
    profiles: &Profiles,
    levels: &Levels,
    deps: bool,
) -> Vec<Diagnostic> {
    let profile_of = file_profiles(modules, sm, profiles);
    let of_span = |s: Span| {
        profile_of
            .get(&s.file)
            .copied()
            .unwrap_or(Profile::Dependency)
    };
    let mut seen = HashSet::new();
    let mut out = vec![];
    for item in items {
        let wanted = velt_tscompat::canonical(&item.file);
        let Some((file, f)) = sm
            .files()
            .find(|(_, f)| velt_tscompat::canonical(&f.path) == wanted)
        else {
            continue;
        };
        let Some(module) = modules.iter().find(|m| m.file == file) else {
            continue;
        };
        let at = offset(&f.src, item.line, item.col);
        let used = Span::new(file, at, at + item.name.len() as u32);
        let decl = analysis
            .def_at(file, at)
            .filter(|d| d.name == item.name && d.kind == DefKind::Local && d.span.file == file)
            .map(|d| d.span);
        if !seen.insert((file, decl.unwrap_or(used))) {
            continue;
        }
        let finding = finding(item, module, f.src.as_str(), used, decl);
        let profile = of_span(used);
        if profile == Profile::Dependency {
            if deps {
                let mut d =
                    crate::report(finding, Profile::Native, &|_| Profile::Dependency, levels);
                if let Some(d) = d.as_mut() {
                    d.severity = velt_common::Severity::Note;
                    d.notes
                        .insert(0, "in a dependency: report it to its authors".into());
                }
                out.extend(d);
            }
            continue;
        }
        out.extend(crate::report(finding, profile, &of_span, levels));
    }
    out
}

fn finding(
    item: &NumberDouble,
    module: &SourceModule,
    src: &str,
    used: Span,
    decl: Option<Span>,
) -> Finding {
    let name = &item.name;
    let mut notes = vec![
        "a double costs more than an integer in a loop: a conversion at each array index, \
         float adds and compares"
            .to_string(),
    ];
    let mut fixes = vec![];
    if item.unbounded {
        notes.push(format!(
            "an integer type holds it exactly: `{name}: i64` (it wraps past 2^63 where a `number` \
             loses precision past 2^53; where it meets a `number`, `as number` converts it); in \
             TypeScript too, `(…) | 0` keeps it a 32-bit integer"
        ));
        if let Some(fix) = decl.and_then(|d| annotate(module, src, d)) {
            fixes.push(ProfiledFix {
                fix: Fix::replace(
                    format!("declare `{name}` as `i64`"),
                    fix.0,
                    fix.1,
                    Applicability::Review,
                ),
                portable: false,
            });
        }
    }
    let span = decl.unwrap_or(used);
    let mut f = Finding {
        code: NUMBER_DOUBLE,
        span,
        message: format!("`{name}` stays a double in this loop: {}", item.reason),
        notes,
        fixes,
    };
    if decl.is_some() && used != span {
        f.notes.insert(
            0,
            format!("it is assigned in the loop at line {}", item.line),
        );
    }
    f
}

/// The edit that declares the local whose name is at `decl` as `i64`: a type after the name,
/// or `i64` for a written `number`.
fn annotate(module: &SourceModule, src: &str, decl: Span) -> Option<(Span, String)> {
    struct Find {
        decl: Span,
        found: Option<(Span, Option<Span>)>,
    }
    impl<'a> Visit<'a> for Find {
        fn var_decl(&mut self, v: &'a ast::VarDecl) {
            if let ast::PatternKind::Ident(id) = &v.pattern.kind {
                if id.span == self.decl && v.kind == ast::VarKind::Let {
                    self.found = Some((id.span, v.ty.as_ref().map(|t| t.span)));
                }
            }
        }
    }
    let mut find = Find { decl, found: None };
    walk_module(&module.ast, &mut find);
    match find.found? {
        (name, None) => Some((Span::new(name.file, name.hi, name.hi), ": i64".into())),
        (_, Some(ty)) if src.get(ty.lo as usize..ty.hi as usize) == Some("number") => {
            Some((ty, "i64".into()))
        }
        _ => None,
    }
}

/// The byte offset of 1-based `line` and `col` in `src`.
fn offset(src: &str, line: u32, col: u32) -> u32 {
    let start: usize = src
        .split_inclusive('\n')
        .take(line.saturating_sub(1) as usize)
        .map(str::len)
        .sum();
    (start + col.saturating_sub(1) as usize).min(src.len()) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        let src = "ab\ncde\nf";
        assert_eq!(offset(src, 1, 1), 0);
        assert_eq!(offset(src, 2, 2), 4);
        assert_eq!(offset(src, 3, 1), 7);
        assert_eq!(offset(src, 9, 9), 8);
    }
}
