//! `<T>x`, TypeScript's older type assertion syntax. In a `.tsx` or `.vlt` file a `<` there
//! starts JSX, as in TypeScript's `.tsx`; in a plain `.ts` file it is a type assertion, which
//! Velt does not have: it is reported with a fix, and the operand is parsed on so the rest of the
//! file still parses.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;
use velt_common::Diagnostic;

impl Parser<'_> {
    /// At a `<` that could start JSX in a plain `.ts` file: `<Type>operand` is reported and its
    /// operand returned. `None` (nothing consumed) when it reads as an element instead: not
    /// `<Type>`, or a closing tag `</Name` for `<Name>` follows (the loader then reports JSX in a
    /// `.ts` file).
    pub(super) fn try_type_assertion(&mut self) -> PResult<Option<Expr>> {
        let lo = self.cur_lo();
        let snap = self.snapshot();
        let Some(ty) = self.speculate(|p| {
            p.bump(); // <
            let ty = p.parse_type()?;
            p.expect(Tok::Gt)?;
            Ok(ty)
        }) else {
            return Ok(None);
        };
        if let TypeExprKind::Named { path, args } = &ty.kind {
            let name: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            let closing = format!("</{}", name.join("."));
            if args.is_empty() && self.src[self.prev_hi as usize..].contains(&closing) {
                self.restore(snap);
                return Ok(None);
            }
        }
        let span = self.span_from(lo);
        self.diags.push(
            Diagnostic::error("type assertions `<T>x` are not supported", span)
                .with_note("narrow with `typeof`/`instanceof`, or annotate the variable's type"),
        );
        self.parse_unary().map(Some)
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{ExprKind, ItemKind};
    use velt_common::FileId;

    fn messages(src: &str, ts: bool) -> Vec<String> {
        let (_, diags) = if ts {
            crate::parse_ts_file(FileId(0), src)
        } else {
            crate::parse_file(FileId(0), src)
        };
        diags.into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn a_type_assertion_in_a_ts_file_is_reported_and_its_operand_parsed() {
        let src = "const a = <number>x;\nconst b = <Array<string>>y.z;\nfunction f() {}\n";
        assert_eq!(
            messages(src, true),
            [
                "type assertions `<T>x` are not supported",
                "type assertions `<T>x` are not supported"
            ]
        );
        let (module, _) = crate::parse_ts_file(FileId(0), src);
        assert_eq!(module.items.len(), 3, "the rest of the file still parses");
        let ItemKind::Var(decl) = &module.items[0].kind else {
            panic!("{:?}", module.items[0]);
        };
        let init = decl.init.as_ref().map(|e| &e.kind);
        assert!(matches!(init, Some(ExprKind::Ident(_))), "{init:?}");
    }

    #[test]
    fn elements_stay_jsx_in_ts_files_and_tsx_is_unchanged() {
        // With a closing tag, or not of the form `<Type>`: an element (the loader reports JSX
        // in a `.ts` file).
        for src in [
            "const a = <b>label</b>;\n",
            "const a = <br />;\n",
            "const a = <div class=\"x\"></div>;\n",
            "const f = <T>(x: T): T => x;\n",
        ] {
            assert_eq!(messages(src, true), Vec::<String>::new(), "{src}");
        }
        let msgs = messages("const a = <number>x;\n", false);
        assert!(msgs[0].contains("no corresponding closing tag"), "{msgs:?}");
    }
}
