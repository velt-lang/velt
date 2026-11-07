//! `import("m").T` in a type (TypeScript's import type): the type `T` of module `m` without an
//! import declaration. The parser adds `import type * as <name> from "m"` to the module and the
//! type is the path `<name>.T`, so the loader finds the module and sema resolves the path as it
//! does through `import * as`. `<name>` is the written form, `import("m")`, which no declared
//! name can be: messages and `velt fmt` show the type as it was written.

use velt_common::Span;

use super::{Fail, PResult, Parser};
use crate::ast::{Ident, Import, Item, ItemKind};
use crate::lexer::{Kw, Tok};

impl Parser<'_> {
    /// At `import(` in a type?
    pub(super) fn at_import_type(&mut self) -> bool {
        self.at(Tok::Kw(Kw::Import)) && self.nth(1) == Tok::LParen
    }

    /// `import("m")` (the cursor at `import`): the name of the module's namespace, a segment of
    /// the type's path.
    pub(super) fn parse_import_type_head(&mut self) -> PResult<Ident> {
        let lo = self.cur_lo();
        self.bump(); // import
        self.expect(Tok::LParen)?;
        let from_span = self.cur_span();
        let Tok::Str(idx) = self.peek() else {
            self.error_expected("a module name");
            return Err(Fail);
        };
        let from = self.payload_text(idx);
        self.bump();
        self.expect(Tok::RParen)?;
        let span = self.span_from(lo);
        let name = format!("import({from:?})");
        if !self.import_types.iter().any(|(n, _, _)| *n == name) {
            self.import_types.push((name.clone(), from, from_span));
        }
        Ok(Ident { name, span })
    }

    /// The imports the module's import types need.
    pub(super) fn import_type_items(&mut self) -> Vec<Item> {
        let at = Span::new(self.file, 0, 0);
        let imports = std::mem::take(&mut self.import_types);
        (imports.into_iter())
            .map(|(name, from, from_span)| Item {
                kind: ItemKind::Import(Import {
                    names: vec![],
                    from,
                    from_span,
                    namespace: Some(Ident { name, span: at }),
                    all: false,
                    default_import: false,
                    type_only: true,
                }),
                exported: false,
                span: at,
            })
            .collect()
    }
}
