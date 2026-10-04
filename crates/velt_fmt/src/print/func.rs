//! Functions: declarations and signatures (shared by methods and `declare function`), parameter
//! lists, generic parameter lists and arrow functions.

use velt_syntax::ast::{ArrowBody, ArrowParam, Expr, FnDecl, FnSig, GenericParam, Param, TypeExpr};

use super::lists::delimited;
use super::Printer;
use crate::doc::{cat, group, indent, join, line, nil, text, Doc};

impl<'a> Printer<'a> {
    /// `[async ]<keyword>name<T>(params): R [throws E] { body }` (`keyword` is `function ` or
    /// empty).
    pub(super) fn fn_decl(&mut self, f: &FnDecl, keyword: &str) -> Doc {
        let asyncness = if f.sig.is_async { "async " } else { "" };
        let sig = self.fn_sig(&f.sig, keyword, f.body.span.lo);
        cat![asyncness, sig, " ", self.block(&f.body)]
    }

    /// Signature without modifiers; `end` bounds the parameter list's comments.
    pub(super) fn fn_sig(&mut self, sig: &FnSig, keyword: &str, end: u32) -> Doc {
        let generics = self.generic_params(&sig.generics);
        let params_end = sig
            .ret
            .as_ref()
            .or(sig.throws.as_ref())
            .map_or(end, |r| r.span.lo);
        let params = self.params(&sig.params, params_end);
        let ret = self.return_type(sig.ret.as_ref());
        let throws = self.throws_clause(sig.throws.as_ref());
        // `function* name` / `*name` for generators.
        let keyword = match (sig.is_generator, keyword) {
            (false, k) => k.to_string(),
            (true, "") => "*".to_string(),
            (true, k) => format!("{}* ", k.trim_end()),
        };
        group(cat![
            keyword,
            sig.name.name.clone(),
            generics,
            params,
            ret,
            throws
        ])
    }

    fn return_type(&mut self, ret: Option<&TypeExpr>) -> Doc {
        match ret {
            Some(ty) => cat![": ", self.ty(ty)],
            None => nil(),
        }
    }

    /// ` throws E`, or nothing.
    pub(super) fn throws_clause(&mut self, throws: Option<&TypeExpr>) -> Doc {
        match throws {
            Some(ty) => cat![" throws ", self.ty(ty)],
            None => nil(),
        }
    }

    fn params(&mut self, params: &[Param], end: u32) -> Doc {
        let list = self.list(
            params,
            end,
            |p| (p.span.lo, p.span.hi),
            |p, param| {
                // Parameter property modifiers (`private readonly x`) precede the name.
                let mods = crate::source::slice(p.src, param.span);
                let mods = mods
                    .get(..(param.name.span.lo - param.span.lo) as usize)
                    .unwrap_or("")
                    .split_whitespace()
                    .filter(|m| *m != "...")
                    .map(|m| format!("{m} "))
                    .collect::<String>();
                if param.optional {
                    return cat![
                        mods,
                        param.name.name.clone(),
                        "?: ",
                        p.ty_optional(&param.ty)
                    ];
                }
                if param.rest {
                    return cat![mods, "...", param.name.name.clone(), ": ", p.ty(&param.ty)];
                }
                let mut doc = cat![mods, param.name.name.clone(), ": ", p.ty(&param.ty)];
                if let Some(default) = &param.default {
                    doc = cat![doc, " = ", p.expr(default)];
                }
                doc
            },
        );
        delimited("(", list, ")", false)
    }

    /// `<T, U extends A & B, E = never>`, or nothing.
    pub(super) fn generic_params(&mut self, generics: &[GenericParam]) -> Doc {
        if generics.is_empty() {
            return nil();
        }
        let docs = generics
            .iter()
            .map(|g| {
                let head = if g.bounds.is_empty() {
                    text(g.name.name.clone())
                } else {
                    let bounds = g.bounds.iter().map(|b| self.ty_no_union(b)).collect();
                    cat![g.name.name.clone(), " extends ", join(&text(" & "), bounds)]
                };
                match &g.default {
                    Some(d) => cat![head, " = ", self.ty(d)],
                    None => head,
                }
            })
            .collect();
        cat!["<", join(&text(", "), docs), ">"]
    }

    /// `[async ][<T>](params)[: R [throws E]] => body`. `<T,>` (the `.tsx` spelling) is printed
    /// as `<T>`: the parser tells a generic arrow from a JSX element.
    pub(super) fn arrow(
        &mut self,
        type_params: &[GenericParam],
        params: &[ArrowParam],
        (ret, throws): (Option<&TypeExpr>, Option<&TypeExpr>),
        body: &ArrowBody,
        is_async: bool,
    ) -> Doc {
        let body_lo = match body {
            ArrowBody::Block(b) => b.span.lo,
            ArrowBody::Expr(e) => e.span.lo,
        };
        let list = self.list(
            params,
            ret.map_or(body_lo, |r| r.span.lo),
            |p| {
                let hi = p.ty.as_ref().map_or(p.name.span.hi, |t| t.span.hi);
                let hi = p.default.as_ref().map_or(hi, |d| d.span.hi.max(hi));
                (p.name.span.lo, hi)
            },
            |p, param| {
                if param.optional {
                    let ty = match &param.ty {
                        Some(ty) => cat![": ", p.ty_optional(ty)],
                        None => nil(),
                    };
                    return cat![param.name.name.clone(), "?", ty];
                }
                let ty = match &param.ty {
                    Some(ty) => cat![": ", p.ty(ty)],
                    None => nil(),
                };
                let default = match &param.default {
                    Some(d) => cat![" = ", p.expr(d)],
                    None => nil(),
                };
                cat![param.name.name.clone(), ty, default]
            },
        );
        let asyncness = if is_async { "async " } else { "" };
        let type_params = self.generic_params(type_params);
        let ret = self.return_type(ret);
        let throws = self.throws_clause(throws);
        let head = cat![
            asyncness,
            type_params,
            delimited("(", list, ")", false),
            ret,
            throws,
            " =>"
        ];
        match body {
            ArrowBody::Block(b) => cat![head, " ", self.block(b)],
            ArrowBody::Expr(e) => self.body_after(head, e),
        }
    }

    /// `head body` where `body` is an expression after `=>`: on the same line if it breaks
    /// nicely by itself (calls, literals, blocks...), else indented on the next line when long.
    pub(super) fn body_after(&mut self, head: Doc, body: &Expr) -> Doc {
        if super::jsx::is_jsx_layout(body) {
            return cat![head, " ", self.expr_jsx_parens(body)];
        }
        if breaks_itself(body) {
            return cat![head, " ", self.expr(body)];
        }
        group(cat![head, indent(cat![line(), self.expr_no_indent(body)])])
    }
}

/// Expressions whose own layout breaks well (after an opening bracket), so they can start on
/// the line of the `=`/`=>` in front of them.
pub(super) fn breaks_itself(e: &Expr) -> bool {
    use velt_syntax::ast::ExprKind::*;
    match &e.kind {
        Call { .. } | New { .. } | Object(_) | StructLit { .. } | Array(_) | Arrow { .. } => true,
        Function(_) => true,
        Template { .. } => true,
        Await(inner) | Paren(inner) => breaks_itself(inner),
        _ => false,
    }
}
