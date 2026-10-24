//! `static { … }` blocks in a class body (ES2022): each runs once, where the class is declared,
//! in source order with the static field initializers. The parser gives a block the form of a
//! static field of type `void` whose initializer calls an arrow with the block as its body,
//! `static <static block N>: void = (() => { … })();` (N counts the blocks of the file; no
//! source name can be that); a `static` field that is not `readonly` is evaluated once, in
//! source order, like a module-level `let`.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl Parser<'_> {
    /// At `static {`?
    pub(super) fn at_static_block(&mut self) -> bool {
        self.at_kw(Kw::Static) && self.nth(1) == Tok::LBrace
    }

    /// `static { stmts }` (the cursor at `static`) as the field that runs it.
    pub(super) fn parse_static_block(&mut self, lo: u32) -> PResult<Field> {
        let kw = self.cur_span();
        let n = self.static_blocks;
        self.static_blocks += 1;
        self.bump(); // static
        let body = self.parse_fn_body()?;
        let span = self.span_from(lo);
        let void = TypeExpr {
            kind: TypeExprKind::Void,
            span: kw,
        };
        let arrow = ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: Some(void.clone()),
            throws: None,
            body: ArrowBody::Block(body),
            is_async: false,
        };
        let arrow = self.mk_expr(arrow, span);
        let callee = self.mk_expr(ExprKind::Paren(Box::new(arrow)), span);
        let call = ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![],
            optional: false,
        };
        let call = self.mk_expr(call, span);
        Ok(Field {
            name: Ident {
                name: format!("<static block {n}>"),
                span: kw,
            },
            ty: void,
            default: Some(call),
            readonly: false,
            optional: false,
            is_private: false,
            is_static: true,
            span,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{ExprKind, ItemKind};
    use velt_common::FileId;

    #[test]
    fn a_static_block_is_a_static_field_that_calls_its_body() {
        let src = "class A {\n  static n: number = 1;\n  static { A.n += 1; }\n  static() {}\n}\n";
        let (module, diags) = crate::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let ItemKind::Class(c) = &module.items[0].kind else {
            panic!("{:?}", module.items[0]);
        };
        let names: Vec<&str> = c.fields.iter().map(|f| f.name.name.as_str()).collect();
        assert_eq!(names, ["n", "<static block 0>"]);
        let block = &c.fields[1];
        assert!(block.is_static && !block.readonly);
        let Some(ExprKind::Call { callee, args, .. }) = block.default.as_ref().map(|e| &e.kind)
        else {
            panic!("{:?}", block.default);
        };
        assert!(args.is_empty());
        assert!(
            matches!(&callee.kind, ExprKind::Paren(a) if matches!(a.kind, ExprKind::Arrow { .. }))
        );
        // `static() {}` is a method named `static`.
        assert_eq!(c.methods.len(), 1);
    }
}
