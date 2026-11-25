//! A class that extends `RegExp` and declares `[Symbol.match]`, `[Symbol.matchAll]`,
//! `[Symbol.replace]`, `[Symbol.search]` or `[Symbol.split]` is refused where it declares it.
//! JavaScript's string methods call these methods of a regex; Velt's go to std's hooks
//! (`body::expr::regex_args`), which call `RegExp`'s own under internal keys, so the class's
//! method would never run and `s.split(re)` would answer as if it were not there (#3823).

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::classes::base_of_def;
use crate::ctx::Ctx;
use crate::hir::DefId;

/// The methods of a regex JavaScript's string methods call, as a class writes their names.
const METHODS: [&str; 5] = [
    "[Symbol.match]",
    "[Symbol.matchAll]",
    "[Symbol.replace]",
    "[Symbol.search]",
    "[Symbol.split]",
];

/// Reports the method or accessor `name` of class `d` when it is one of [`METHODS`] and `d` is
/// a program's class that extends std's `RegExp`.
pub(super) fn check(cx: &mut Ctx, d: DefId, name: &ast::Ident) {
    if !METHODS.contains(&name.name.as_str()) || cx.declared_in_std(d) || !extends_regexp(cx, d) {
        return;
    }
    let method = name
        .name
        .trim_start_matches("[Symbol.")
        .trim_end_matches(']');
    let call = match method {
        "replace" => "s.replace(re, …)".to_string(),
        _ => format!("s.{method}(re)"),
    };
    cx.error(
        Diagnostic::error(
            format!(
                "`{}` in a class that extends `RegExp` is not supported yet: `{call}` would not call it",
                name.name
            ),
            name.span,
        )
        .with_note("JavaScript's string methods call this method of the regex; Velt's call the regex's `exec` for every match, so declare `exec` to change what matches (velt-lang/velt#3823)"),
    );
}

/// Is std's `RegExp` an ancestor of class `d`?
fn extends_regexp(cx: &Ctx, d: DefId) -> bool {
    let regexp = velt_common::std_item!(text_regex_regexp, "RegExp");
    let mut seen = 0;
    let mut cur = base_of_def(cx, d);
    while let Some(b) = cur {
        if cx.adt(b).is_some_and(|a| a.qual_name == regexp) {
            return true;
        }
        seen += 1;
        if seen > 1024 {
            return false;
        }
        cur = base_of_def(cx, b);
    }
    false
}
