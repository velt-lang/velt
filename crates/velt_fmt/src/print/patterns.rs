//! Patterns: bindings and destructuring (`{ a, b: c, ...rest }`, `[a, _, ...rest]`). Array holes
//! are printed as `_` (the parser turns both into a wildcard).

use velt_syntax::ast::{Ident, Pattern, PatternKind};

use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, softline, text, Doc};

impl<'a> Printer<'a> {
    pub(super) fn pattern(&mut self, p: &Pattern) -> Doc {
        match &p.kind {
            PatternKind::Ident(id) => text(id.name.clone()),
            PatternKind::Wildcard => "_".into(),
            PatternKind::Object { fields, rest } => self.object_pattern(fields, rest.as_ref()),
            PatternKind::Array { elems, rest } => {
                let mut docs: Vec<Doc> = elems.iter().map(|e| self.pattern(e)).collect();
                docs.extend(rest.iter().map(|r| text(format!("...{}", r.name))));
                bracketed("[", docs, "]", softline())
            }
        }
    }

    fn object_pattern(&mut self, fields: &[(Ident, Pattern)], rest: Option<&Ident>) -> Doc {
        let mut docs: Vec<Doc> = fields
            .iter()
            .map(|(key, pat)| match &pat.kind {
                PatternKind::Ident(id) if id.name == key.name => text(key.name.clone()),
                _ => cat![key.name.clone(), ": ", self.pattern(pat)],
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
