//! String methods called with a regex: `s.replace(/a/g, "b")`, `s.replace(re, (m, p1) => …)`,
//! `match`, `matchAll`, `search`, `split` and `replaceAll`. Velt has no overloads, so a call
//! whose first argument is std's `RegExp` goes to the method of std/regex's `extend string`
//! block named after it (`__replaceRegExp`, `__matchRegExp`, …); the string-argument methods
//! stay as they are, at no cost.
//!
//! A replacer function gets JavaScript's arguments: the match, the groups, the match's offset
//! and the subject. std calls `(m: RegExpMatch, s: string) => string`, so the function is
//! adapted: `(m, p1, offset) => body` becomes `(<match>, <subject>) => { let m = <match>.value;
//! let p1 = <match>[1]; let offset = <match>.index as number; body }`. Which parameter is the
//! offset depends on the number of groups, known for a regex literal (or `new RegExp` of
//! literals) and for a `const` local or `readonly` field initialized with one;
//! for another regex a replacer taking more than the match is an error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E};

use super::method_value::{is_path, strip_parens};
use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

/// The string methods that take a regex in place of their first argument.
const METHODS: &[&str] = &[
    "replace",
    "replaceAll",
    "match",
    "matchAll",
    "search",
    "split",
];

/// The name of the replacer's match parameter in the adapter.
const MATCH: &str = "<match>";
/// The name of the replacer's subject parameter in the adapter.
const SUBJECT: &str = "<subject>";

impl FnCx<'_, '_> {
    /// `s.<prop>(re, …)` with a `RegExp` first argument: the std method to call instead, and its
    /// arguments (a replacer function adapted). `None` for any other call.
    pub(super) fn regex_string_call(
        &mut self,
        prop: &ast::Ident,
        args: &[ast::Expr],
    ) -> Option<(ast::Ident, Vec<ast::Expr>)> {
        if !METHODS.contains(&prop.name.as_str()) {
            return None;
        }
        let shape = match args.first() {
            Some(a) if self.is_regexp_arg(a) => self.regex_shape(a),
            _ => return None,
        };
        let needs_g = matches!(prop.name.as_str(), "matchAll" | "replaceAll");
        if shape
            .as_ref()
            .is_some_and(|(_, f)| needs_g && !f.contains('g'))
        {
            self.cx.error(
                Diagnostic::error(
                    format!("`{}` needs a regex with the `g` flag", prop.name),
                    args[0].span,
                )
                .with_note("JavaScript throws a TypeError for a regex without it: add `g`"),
            );
        }
        let mut name = format!("__{}RegExp", prop.name);
        let mut out = args.to_vec();
        if let (true, Some(f)) = (prop.name.starts_with("replace"), args.get(1)) {
            let groups = shape.map(|(n, _)| n);
            if let Some(adapter) = self.replacer_adapter(f, groups, &args[0]) {
                out[1] = adapter;
                name.push_str("Calling");
            }
        }
        let ident = ast::Ident {
            name,
            span: prop.span,
        };
        Some((ident, out))
    }

    /// Is `e` a `RegExp`: `new RegExp(…)` (a regex literal is one) or a variable or field path
    /// of that type? Other expressions take the string method (and its type errors).
    fn is_regexp_arg(&mut self, e: &ast::Expr) -> bool {
        let ty = match &strip_parens(e).kind {
            E::New { class, .. } => {
                let mark = self.cx.diags.len();
                let t = self.resolve(class);
                self.cx.diags.truncate(mark);
                Some(t)
            }
            _ => self.peek_ty(e),
        };
        ty.is_some_and(|t| self.is_std_class(t, "std/regex::RegExp"))
    }

    /// Is `t` the std class with the qualified name `qual`?
    pub(super) fn is_std_class(&self, t: TyId, qual: &str) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self.cx.adt(*d).is_some_and(|a| a.qual_name == qual),
            _ => false,
        }
    }

    /// The number of capturing groups and the flags of the regex `e`, when known while
    /// compiling: a regex literal (or `new RegExp` of literals), a `const` local holding one
    /// (`Frame::regex_consts`), or a `readonly` field initialized with one that its class's
    /// constructor does not assign.
    pub(in crate::body) fn regex_shape(&mut self, e: &ast::Expr) -> Option<(usize, String)> {
        let e = strip_parens(e);
        if let Some(shape) = literal_shape(e) {
            return Some(shape);
        }
        match &e.kind {
            E::Ident(id) => self.peek_regex_local(&id.name),
            E::Member {
                object,
                prop,
                optional: false,
            } => self.field_regex(object, prop),
            _ => None,
        }
    }

    /// `const re = …` of type `ty`: what `regex_shape` knows of the regex it holds.
    pub(in crate::body) fn regex_const_shape(
        &mut self,
        init: &ast::Expr,
        ty: TyId,
    ) -> Option<(usize, String)> {
        if !self.is_std_class(ty, "std/regex::RegExp") {
            return None;
        }
        self.regex_shape(init)
    }

    /// The regex field `object.prop` holds, when it is `readonly` and initialized with a
    /// literal that its class's constructor does not assign. (A module constant cannot hold a
    /// regex.)
    fn field_regex(&mut self, object: &ast::Expr, prop: &ast::Ident) -> Option<(usize, String)> {
        let ty = self.peek_ty(object)?;
        let mut d = match self.cx.ty.kind(ty) {
            TyKind::Adt(d, _) => *d,
            _ => return None,
        };
        loop {
            let a = self.cx.adt(d)?;
            let decl = a.decl?;
            let field = decl
                .fields
                .iter()
                .find(|f| !f.is_static && f.name.name == prop.name);
            if let Some(f) = field {
                let assigned = decl.constructor.as_ref().is_some_and(|c| {
                    crate::body::assigned::this_fields_assigned(&c.body.stmts)
                        .contains_key(prop.name.as_str())
                });
                return match (f.readonly, assigned, &f.default) {
                    (true, false, Some(init)) => literal_shape(init),
                    _ => None,
                };
            }
            d = match self.cx.ty.kind(a.base?) {
                TyKind::Adt(b, _) => *b,
                _ => return None,
            };
        }
    }

    /// The replacer `f` as the `(m: RegExpMatch, s: string) => string` std calls, when it is a
    /// function: an arrow literal, or a variable holding a function. `groups`: the number of
    /// groups of the regex `re`, when known.
    fn replacer_adapter(
        &mut self,
        f: &ast::Expr,
        groups: Option<usize>,
        re: &ast::Expr,
    ) -> Option<ast::Expr> {
        let span = f.span;
        let (params, ret, throws, body) = match &strip_parens(f).kind {
            E::Arrow {
                type_params,
                params,
                ret,
                throws,
                body,
                is_async: false,
            } if type_params.is_empty() => (params.clone(), ret.clone(), throws.clone(), body),
            _ => return self.replacer_value_adapter(f, groups, re),
        };
        if params.len() > 1 && groups.is_none() {
            // Reported; the parameters are then all groups, as a function written for this
            // regex most likely takes them (no further errors).
            self.unknown_groups(re, span);
        }
        let mut stmts = vec![];
        for (i, p) in params.iter().enumerate() {
            let value = self.replacer_arg(i, p.optional, p.default.as_ref(), groups, span);
            // `let`: the function may assign its parameters.
            let decl = ast::VarDecl {
                kind: ast::VarKind::Let,
                pattern: ast::Pattern {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::PatternKind::Ident(p.name.clone()),
                    span: p.name.span,
                },
                ty: p.ty.clone(),
                init: Some(value),
                span,
            };
            stmts.push(stmt(ast::StmtKind::Var(decl), span));
        }
        match body {
            ast::ArrowBody::Expr(e) => {
                stmts.push(stmt(ast::StmtKind::Return(Some((**e).clone())), span))
            }
            ast::ArrowBody::Block(b) => stmts.extend(b.stmts.iter().cloned()),
        }
        Some(adapter(&ret, &throws, ast::Block { stmts, span }, span))
    }

    /// The adapter for a variable holding a function: it calls the function with as many of
    /// JavaScript's arguments as the function takes.
    fn replacer_value_adapter(
        &mut self,
        f: &ast::Expr,
        groups: Option<usize>,
        re: &ast::Expr,
    ) -> Option<ast::Expr> {
        if !is_path(f) {
            return None;
        }
        let ty = self.peek_ty(f)?;
        let arity = match self.cx.ty.kind(ty) {
            TyKind::FnPtr { params, .. } => params.len(),
            TyKind::Closure(d) => self.cx.fn_info(*d).params.len(),
            _ => return None,
        };
        let span = f.span;
        let groups = match groups {
            None if arity > 1 => {
                // Reported; the function's parameters are then the match, the offset and the
                // string, as a function declared apart most likely takes them.
                self.unknown_groups(re, span);
                Some(0)
            }
            g => g,
        };
        let args = (0..arity)
            .map(|i| self.replacer_arg(i, false, None, groups, span))
            .collect();
        let call = expr(
            E::Call {
                callee: Box::new(f.clone()),
                type_args: vec![],
                args,
                optional: false,
            },
            span,
        );
        let body = ast::Block {
            stmts: vec![stmt(ast::StmtKind::Return(Some(call)), span)],
            span,
        };
        Some(adapter(&None, &None, body, span))
    }

    /// A replacer taking more than the match for a regex whose number of groups is not known
    /// while compiling: which parameter is the offset cannot be told.
    fn unknown_groups(&mut self, re: &ast::Expr, span: Span) {
        let d = Diagnostic::error(
            "this function takes more than the match, but the number of groups of the regex is \
             not known while compiling",
            span,
        )
        .with_label(re.span, "a regex that is not a literal")
        .with_note(
            "JavaScript passes the match, each group, the offset and the string: which \
             parameter is the offset depends on the number of groups",
        )
        .with_note(
            "pass a regex literal, or a `const` or `readonly` field initialized with one \
             (`const re = /(\\w)(\\d)/g;`); or take only the match and read the groups and \
             the offset with the regex's `exec`",
        );
        self.cx.error(d);
    }

    /// JavaScript's argument `i` of a replacer: the match, a group (`null` when it did not take
    /// part if the parameter is optional, else ""), the offset, then the subject. A parameter's
    /// default `dflt` stands for a group that did not take part and for an argument JavaScript
    /// does not pass.
    fn replacer_arg(
        &mut self,
        i: usize,
        optional: bool,
        dflt: Option<&ast::Expr>,
        groups: Option<usize>,
        span: Span,
    ) -> ast::Expr {
        let m = || ident_expr(MATCH, span);
        let n = groups.unwrap_or(usize::MAX);
        let index = || {
            expr(
                E::Lit(ast::Lit::Int {
                    value: i as u128,
                    suffix: None,
                }),
                span,
            )
        };
        match i {
            0 => member(m(), "value", span),
            _ if i <= n => match dflt {
                Some(d) => expr(
                    E::Binary {
                        op: ast::BinaryOp::Nullish,
                        lhs: Box::new(call_method(m(), "group", vec![index()], span)),
                        rhs: Box::new(d.clone()),
                    },
                    span,
                ),
                None => {
                    let method = if optional { "group" } else { "__index" };
                    call_method(m(), method, vec![index()], span)
                }
            },
            _ if i == n + 1 => expr(
                E::Cast {
                    expr: Box::new(member(m(), "index", span)),
                    ty: named_type("number", span),
                },
                span,
            ),
            _ if i == n + 2 => ident_expr(SUBJECT, span),
            _ if dflt.is_some() => dflt.cloned().expect("ICE: checked"),
            _ => {
                if i == n + 3 {
                    let d = Diagnostic::error(
                        format!("this function takes more than the {i} arguments it is given"),
                        span,
                    )
                    .with_note(format!(
                        "for this regex `replace` passes the match, {n} group(s), the offset and \
                         the string; JavaScript's named-groups object is not passed (read named \
                         groups with the regex's `exec`)"
                    ));
                    self.cx.error(d);
                }
                ident_expr(SUBJECT, span)
            }
        }
    }
}

/// `(<match>, <subject>): ret throws E => { body }`: the parameters are typed from std's
/// function type.
fn adapter(
    ret: &Option<ast::TypeExpr>,
    throws: &Option<ast::TypeExpr>,
    body: ast::Block,
    span: Span,
) -> ast::Expr {
    let param = |name: &str| ast::ArrowParam {
        name: ast::Ident {
            name: name.into(),
            span,
        },
        ty: None,
        default: None,
        optional: false,
    };
    expr(
        E::Arrow {
            type_params: vec![],
            params: vec![param(MATCH), param(SUBJECT)],
            ret: ret.clone(),
            throws: throws.clone(),
            body: ast::ArrowBody::Block(body),
            is_async: false,
        },
        span,
    )
}

/// The number of groups and the flags of `new RegExp("…", "…")` with literal arguments.
fn literal_shape(e: &ast::Expr) -> Option<(usize, String)> {
    regex_literal(e).map(|(source, flags)| (capture_groups(source), flags.to_string()))
}

/// The pattern and flags of `new RegExp("…", "…")` with literal arguments (a regex literal).
fn regex_literal(e: &ast::Expr) -> Option<(&str, &str)> {
    let E::New { args, .. } = &strip_parens(e).kind else {
        return None;
    };
    fn text(a: &ast::Expr) -> Option<&str> {
        match &a.kind {
            E::Lit(ast::Lit::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }
    match args.as_slice() {
        [p] => Some((text(p)?, "")),
        [p, f] => Some((text(p)?, text(f)?)),
        _ => None,
    }
}

/// The number of capturing groups in a JavaScript pattern: each `(` that is not escaped, not in
/// a class and not `(?:`, `(?=`, `(?!`, `(?<=` or `(?<!`.
pub(super) fn capture_groups(source: &str) -> usize {
    let b = source.as_bytes();
    let (mut i, mut n, mut class) = (0, 0, false);
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'[' => class = true,
            b']' => class = false,
            b'(' if !class => {
                let named = b.get(i + 1) == Some(&b'?')
                    && b.get(i + 2) == Some(&b'<')
                    && !matches!(b.get(i + 3), Some(b'=' | b'!'));
                if b.get(i + 1) != Some(&b'?') || named {
                    n += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    n
}

fn expr(kind: E, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn stmt(kind: ast::StmtKind, span: Span) -> ast::Stmt {
    ast::Stmt { kind, span }
}

fn ident_expr(name: &str, span: Span) -> ast::Expr {
    expr(
        E::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

fn member(object: ast::Expr, prop: &str, span: Span) -> ast::Expr {
    expr(
        E::Member {
            object: Box::new(object),
            prop: ast::Ident {
                name: prop.into(),
                span,
            },
            optional: false,
        },
        span,
    )
}

fn call_method(object: ast::Expr, method: &str, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    expr(
        E::Call {
            callee: Box::new(member(object, method, span)),
            type_args: vec![],
            args,
            optional: false,
        },
        span,
    )
}

fn named_type(name: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: name.into(),
                span,
            }],
            args: vec![],
        },
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::capture_groups;

    #[test]
    fn counts_capturing_groups() {
        assert_eq!(capture_groups("a(b)(c)"), 2);
        assert_eq!(capture_groups(r"\((x)\)"), 1);
        assert_eq!(capture_groups("[(](?:a)(?=b)(?!c)(?<=d)(?<!e)"), 0);
        assert_eq!(capture_groups("(?<year>\\d+)-(\\d+)"), 2);
        assert_eq!(capture_groups(r"[\]()](a)"), 1);
    }
}
