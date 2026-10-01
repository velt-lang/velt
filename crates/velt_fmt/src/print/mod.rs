//! AST → [`Doc`]: the formatting rules. One file per concern: [`items`] and [`decls`] (top-level
//! declarations), [`stmt`], [`for_loop`], [`expr`], [`binary`], [`call`] (arguments, last-argument hugging),
//! [`chain`] (member chains), [`literals`] (objects, arrays, strings), [`func`] (signatures,
//! parameters, arrows), [`switch`], [`patterns`], [`types`], [`jsx`] and [`jsx_children`] (JSX
//! elements and their children); [`lists`] places comments inside
//! line-per-entry bodies and delimited lists.
//!
//! Comments are interleaved by byte position: every node flushes the not-yet-printed comments
//! that precede it, and list printers attach the comments that end an entry's line to that entry.

mod binary;
mod call;
mod chain;
mod decls;
mod expr;
mod for_loop;
mod func;
mod items;
mod jsx;
mod jsx_children;
mod lists;
mod literals;
mod patterns;
mod stmt;
mod switch;
mod types;

use velt_syntax::ast::Module;

use crate::comments::{Comment, Comments};
use crate::doc::{cat, concat, hardline, nil, text, Doc};

/// Builds the document for one source file.
pub(crate) struct Printer<'a> {
    src: &'a str,
    comments: Comments,
}

impl<'a> Printer<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Printer {
            src,
            comments: Comments::new(src),
        }
    }

    /// The whole file: items separated by blank lines, ending with a newline.
    pub(crate) fn module(&mut self, module: &Module) -> Doc {
        let end = self.src.len() as u32;
        let body = self.lines(
            &module.items,
            end,
            |i| (i.span.lo, i.span.hi),
            items::blank_between,
            |p, i| p.item(i),
        );
        if body.is_nil() {
            return nil();
        }
        cat![body, hardline()]
    }

    /// Comments printed before a node: each followed by a newline (line comments and comments
    /// that ended their line in the source) or a space.
    fn leading_doc(&self, comments: &[Comment]) -> Doc {
        let parts = comments
            .iter()
            .map(|c| {
                let after = if c.needs_newline() {
                    hardline()
                } else {
                    text(" ")
                };
                cat![text(c.text.clone()), after]
            })
            .collect();
        concat(parts)
    }

    /// Flushes the comments before `pos` and prefixes them to `doc`.
    fn with_leading(&mut self, pos: u32, doc: impl FnOnce(&mut Self) -> Doc) -> Doc {
        let leading = self.comments.take_before(pos);
        let doc = doc(self);
        if leading.is_empty() {
            return doc;
        }
        cat![self.leading_doc(&leading), doc]
    }
}
