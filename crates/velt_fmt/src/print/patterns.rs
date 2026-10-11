//! Patterns: bindings and destructuring (`{ a, b: c, ...rest }`, `[a, , ...rest]`). The parser
//! turns both an array hole and `_` into a wildcard; a hole is the one with an empty span, and
//! stays a hole (`_` would be a binding in TS, which may shadow or repeat a name).

use velt_syntax::ast::{Ident, Pattern, PatternKind};

use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, nil, softline, text, Doc};

impl<'a> Printer<'a> {
    pub(super) fn pattern(&mut self, p: &Pattern) -> Doc {
        match &p.kind {
            PatternKind::Ident(id) => text(id.name.clone()),
            PatternKind::Wildcard if is_hole(p) => nil(),
            PatternKind::Wildcard => "_".into(),
            PatternKind::Object { fields, rest } => self.object_pattern(fields, rest.as_ref()),
            PatternKind::Array { elems, rest } => {
                let mut docs: Vec<Doc> = elems.iter().map(|e| self.pattern(e)).collect();
                // A hole last needs a comma after it (`[a, , ]`), or it is gone.
                if rest.is_none() && elems.last().is_some_and(is_hole) {
                    docs.push(nil());
                }
                docs.extend(rest.iter().map(|r| text(format!("...{}", r.name))));
                bracketed("[", docs, "]", softline())
            }
            PatternKind::Default { pattern, value } => {
                cat![self.pattern(pattern), " = ", self.expr(value)]
            }
        }
    }

    fn object_pattern(&mut self, fields: &[(Ident, Pattern)], rest: Option<&Ident>) -> Doc {
        let mut docs: Vec<Doc> = fields
            .iter()
            .map(|(key, pat)| match &pat.kind {
                PatternKind::Ident(id) if id.name == key.name && !self.quoted(key) => {
                    text(key.name.clone())
                }
                PatternKind::Default { pattern, value }
                    if !self.quoted(key)
                        && matches!(&pattern.kind, PatternKind::Ident(id) if id.name == key.name) =>
                {
                    cat![key.name.clone(), " = ", self.expr(value)]
                }
                _ => cat![self.prop_key(key), ": ", self.pattern(pat)],
            })
            .collect();
        docs.extend(rest.map(|r| text(format!("...{}", r.name))));
        if docs.is_empty() {
            return "{}".into();
        }
        bracketed("{", docs, "}", line())
    }
}

/// `open a, b close`, breaking one entry per line (with a trailing comma) when too long.
fn bracketed(open: &str, docs: Vec<Doc>, close: &str, edge: Doc) -> Doc {
    if docs.is_empty() {
        return cat![open, close];
    }
    group(cat![
        open,
        indent(cat![edge.clone(), join(&cat![",", line()], docs)]),
        if_break(",", ""),
        edge,
        close
    ])
}

/// An array pattern's hole (`[, b]`): a wildcard with an empty span, where `_` has a width.
fn is_hole(p: &Pattern) -> bool {
    matches!(p.kind, PatternKind::Wildcard) && p.span.lo == p.span.hi
}
