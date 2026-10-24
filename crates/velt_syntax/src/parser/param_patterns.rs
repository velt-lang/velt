//! What a parameter binds: a name, or a destructuring pattern (`function f([a, b]: [A, B])`,
//! `xs.map(([k, v]) => …)`, `({ a, b }: P) => …`), which gets a name no source can spell
//! (`#param0`) for the body to destructure. And the type of a parameter written with only a
//! default (`status = 302`), taken from the default as for class fields (`field_types`).

use velt_common::Diagnostic;

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;
use crate::visit::{walk_expr, Visit};

impl Parser<'_> {
    /// The `index`-th parameter's name and, for `[…]` / `{…}`, its pattern.
    pub(super) fn parse_param_binding(
        &mut self,
        index: usize,
        rest: bool,
    ) -> PResult<(Ident, Option<Pattern>)> {
        if !matches!(self.peek(), Tok::LBracket | Tok::LBrace) {
            return Ok((self.parse_binding_ident()?, None));
        }
        // Only a pattern a parameter's `:`, `=`, `?`, `,` or `)` follows: otherwise the `[` / `{`
        // is reported where it stands, so recovery resumes as for any bad parameter.
        let parsed = self.speculate(|p| {
            let pattern = p.parse_binding_pattern()?;
            match p.peek() {
                Tok::Colon | Tok::Eq | Tok::Question | Tok::Comma | Tok::RParen => Ok(pattern),
                _ => Err(Fail),
            }
        });
        let Some(pattern) = parsed else {
            return Ok((self.parse_binding_ident()?, None));
        };
        if rest {
            self.param_error(
                "a rest parameter cannot be destructured yet; write `...xs: T[]` and destructure `xs` in the body",
                pattern.span,
            );
        }
        let name = Ident {
            name: format!("#param{index}"),
            span: pattern.span,
        };
        Ok((name, Some(pattern)))
    }

    /// An error that holds whatever the parse turns out to be, so it is kept while speculating
    /// on an arrow's parameters (a failed speculation drops it, as `check_binding_name` does).
    pub(super) fn param_error(&mut self, msg: &str, span: velt_common::Span) {
        self.diags.push(velt_common::Diagnostic::error(msg, span));
    }

    /// The type a parameter's default states (`= 302` is a `number`), else an error.
    pub(super) fn param_type_of(&mut self, name: &Ident, default: &Expr) -> PResult<TypeExpr> {
        if let Some(t) = super::field_types::stated_type(default) {
            return Ok(TypeExpr {
                span: default.span,
                ..t
            });
        }
        let msg = if name.name.starts_with('#') {
            "cannot infer the type of this parameter from its default; write `pattern: T = ...`"
                .to_string()
        } else {
            format!(
                "cannot infer the type of parameter `{}` from its default; write `{}: T = ...`",
                name.name, name.name
            )
        };
        self.error(msg, default.span);
        Err(Fail)
    }
}

/// One parameter as [`Parser::check_param_defaults`] sees it: its name, its pattern, the
/// default written for it (not the `null` / `[]` an optional or rest parameter gets), and
/// its written type (an arrow's may be left to the default).
pub(super) struct ParamDefault<'p> {
    pub name: &'p Ident,
    pub pattern: Option<&'p Pattern>,
    pub default: Option<&'p Expr>,
    pub ty: Option<&'p TypeExpr>,
}

impl<'p> ParamDefault<'p> {
    pub(super) fn of_param(p: &'p Param) -> Self {
        ParamDefault {
            name: &p.name,
            pattern: p.pattern.as_ref(),
            default: p.default.as_ref().filter(|_| !p.optional && !p.rest),
            ty: Some(&p.ty),
        }
    }

    pub(super) fn of_arrow_param(p: &'p ArrowParam) -> Self {
        ParamDefault {
            name: &p.name,
            pattern: p.pattern.as_ref(),
            default: p.default.as_ref().filter(|_| !p.optional && !p.rest),
            ty: p.ty.as_ref(),
        }
    }
}

/// The written type of a parameter whose default the function applies itself (see
/// [`Parser::check_param_defaults`]): `ty | undefined`, so that a call leaving the argument out
/// passes `undefined`, as for `p?: T`. Sema applies such a default in the function's body, to a
/// passed `undefined` too, where the parameter is a `T` again (`default_prologue`). `any` and
/// `unknown` already hold `undefined`.
pub(super) fn applied_in_callee(ty: TypeExpr) -> TypeExpr {
    let dynamic = matches!(&ty.kind, TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "any" | "unknown"));
    if dynamic {
        return ty;
    }
    super::optional_types::or_undefined(ty)
}

impl Parser<'_> {
    /// Where parameter defaults run, and what they can't do yet. A default is evaluated where
    /// the function is called, before the call. One that uses an earlier parameter
    /// (`b: number = a * 2`), `this` or a closure (`cb: () => number = () => local`, which may
    /// capture the function's surroundings) runs in the function instead, at the start of its
    /// body, as in JavaScript, and so does every later default that is not a constant
    /// (JavaScript runs them in order): the indices returned, whose types the caller makes
    /// `T | undefined` ([`applied_in_callee`]). A `generator`'s defaults all run at the call
    /// (its body starts at the first `next()`). What can't be applied in the function yet is
    /// reported ([`Unmovable`]). A call-site default would also run before the defaults inside
    /// an earlier destructured parameter (`({ x = f() }: P, y = g())`, which run in the body),
    /// where JS runs `f()` first, so after such a pattern a default must be a constant.
    pub(super) fn check_param_defaults(
        &mut self,
        params: &[ParamDefault<'_>],
        generator: bool,
    ) -> Vec<usize> {
        let mut bound: Vec<&str> = vec![];
        let mut plain: Vec<&str> = vec![];
        let mut pattern_defaults = false;
        let mut in_callee: Vec<usize> = vec![];
        for (i, p) in params.iter().enumerate() {
            if let Some(d) = p.default {
                let uses_plain = first_use_of(d, &plain);
                let in_order = !in_callee.is_empty() && !is_constant(d);
                let blocker = if generator {
                    Some(Unmovable::Generator)
                } else if let Some(t) = p.ty {
                    has_null(t).then_some(Unmovable::Null)
                } else {
                    Some(Unmovable::Untyped)
                };
                if let Some(used) = first_use_of(d, &bound) {
                    self.diags
                        .push(earlier_param_error(used, Unmovable::Pattern));
                } else if pattern_defaults && !is_constant(d) {
                    self.diags.push(
                        Diagnostic::error(
                            "a parameter default after a destructured parameter with defaults must be a constant for now",
                            d.span,
                        )
                        .with_note("JavaScript runs the defaults inside the earlier pattern first, but Velt evaluates this default where the function is called, before them; compute the value in the body instead (`q?: number`, then `const r = q ?? f();`)"),
                    );
                } else if let (Some(used), Some(why)) = (uses_plain, blocker) {
                    self.diags.push(earlier_param_error(used, why));
                } else if let (true, Some(why)) = (in_order, blocker) {
                    self.diags.push(out_of_order_error(d, why));
                } else if blocker.is_none() && (uses_plain.is_some() || in_order || runs_in_body(d))
                {
                    in_callee.push(i);
                }
            }
            match p.pattern {
                Some(q) => {
                    pattern_names(q, &mut bound);
                    pattern_defaults |= has_defaults(q);
                }
                None => plain.push(&p.name.name),
            }
        }
        in_callee
    }
}

/// Why a parameter default that must run in the function (it uses an earlier parameter, or
/// follows one that does) can't yet.
#[derive(Clone, Copy)]
enum Unmovable {
    /// It uses a name a destructured parameter binds: the function applies its defaults before
    /// it unpacks those.
    Pattern,
    /// A generator's defaults run where it is called.
    Generator,
    /// The parameter has no written type (an arrow's `(a: number, b = a)`), which the
    /// `T | undefined` callers see needs.
    Untyped,
    /// The parameter's type includes `null`, which the body's `T` would drop with the
    /// `undefined` (`p!`).
    Null,
}

impl Unmovable {
    /// Why, and what to write instead.
    fn note(self) -> &'static str {
        match self {
            Unmovable::Pattern => "Velt applies such a default when the call starts, before it unpacks destructured parameters, so it can't use a name one binds yet; compute the value in the body instead (`q?: number`, then `const r = q ?? p + 1;`)",
            Unmovable::Generator => "a generator's parameter defaults are evaluated where it is called, so they can't use earlier parameters yet; compute the value in the body instead (`q?: number`, then `const r = q ?? p + 1;`)",
            Unmovable::Untyped => "Velt applies such a default when the call starts, which needs the parameter's written type: write `q: T = …`, or take `q?: T` and write `const r = q ?? p + 1;` in the body",
            Unmovable::Null => "Velt applies such a default when the call starts, which it can't do yet for a parameter whose type includes `null`; take `q?: T | null` and write `const r = q === undefined ? p + 1 : q;` in the body",
        }
    }
}

fn earlier_param_error(used: &Ident, why: Unmovable) -> Diagnostic {
    Diagnostic::error(
        format!(
            "a parameter default cannot use an earlier parameter (`{}`) yet",
            used.name
        ),
        used.span,
    )
    .with_note(why.note())
}

/// A non-constant default `d` after one the function applies itself: JavaScript runs it after
/// that one, so it must run in the function too, which `why` prevents.
fn out_of_order_error(d: &Expr, why: Unmovable) -> Diagnostic {
    let what = match why {
        Unmovable::Null => "cannot be on a parameter whose type includes `null` yet",
        _ => "needs the parameter's written type",
    };
    Diagnostic::error(
        format!("a parameter default after one the function applies itself {what}"),
        d.span,
    )
    .with_note(format!(
        "JavaScript runs parameter defaults in order. An earlier default uses an earlier parameter, `this` or a closure, so the function applies it when the call starts, and this one must run after it, there too. {}",
        match why {
            Unmovable::Null => "Velt can't apply it there for a type that includes `null`; take `q?: T | null` and write `const r = q === undefined ? f() : q;` in the body",
            _ => "Write the parameter's type (`q: T = …`)",
        }
    ))
}

/// The names pattern `p` binds, appended to `out`.
fn pattern_names<'p>(p: &'p Pattern, out: &mut Vec<&'p str>) {
    match &p.kind {
        PatternKind::Ident(id) => out.push(&id.name),
        PatternKind::Wildcard => {}
        PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, q)| pattern_names(q, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|q| pattern_names(q, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}

/// Does pattern `p` have a default anywhere (`{ a = 1 }`, `[[x = 0]]`)?
fn has_defaults(p: &Pattern) -> bool {
    match &p.kind {
        PatternKind::Ident(_) | PatternKind::Wildcard => false,
        PatternKind::Object { fields, .. } => fields.iter().any(|(_, q)| has_defaults(q)),
        PatternKind::Array { elems, .. } => elems.iter().any(has_defaults),
        PatternKind::Default { .. } => true,
    }
}

/// A default whose value doesn't depend on when it is evaluated: a literal, or an array or
/// object of them.
fn is_constant(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Lit(_) => true,
        ExprKind::Template { exprs, .. } => exprs.is_empty(),
        ExprKind::Ident(id) => matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity"),
        ExprKind::Unary { expr, .. } | ExprKind::Paren(expr) => is_constant(expr),
        ExprKind::Array(items) => items.iter().all(is_constant),
        ExprKind::Object(props) => props
            .iter()
            .all(|p| matches!(p, ObjectProp::KeyValue(_, v) if is_constant(v))),
        _ => false,
    }
}

/// The first name in `e` that is one of `names` (a variable read, or an object literal's
/// shorthand `{ a }`).
fn first_use_of<'e>(e: &'e Expr, names: &[&str]) -> Option<&'e Ident> {
    struct Uses<'e, 'n> {
        names: &'n [&'n str],
        found: Option<&'e Ident>,
    }
    impl<'e> Visit<'e> for Uses<'e, '_> {
        fn expr(&mut self, e: &'e Expr) {
            let hit = |id: &Ident| self.names.contains(&id.name.as_str());
            let found = match &e.kind {
                ExprKind::Ident(id) if hit(id) => Some(id),
                ExprKind::Object(props) => props.iter().find_map(|p| match p {
                    ObjectProp::Shorthand(id) if hit(id) => Some(id),
                    _ => None,
                }),
                _ => None,
            };
            if self.found.is_none() {
                self.found = found;
            }
        }
    }
    if names.is_empty() {
        return None;
    }
    let mut v = Uses { names, found: None };
    walk_expr(e, &mut v);
    v.found
}

/// Does the written type `ty` include `null` (`T | null`)?
fn has_null(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Null => true,
        TypeExprKind::Union(ms) => ms.iter().any(has_null),
        _ => false,
    }
}

/// Does `e` use `this` (`by = this.x`) or make a closure (`cb = () => local`, which may capture
/// the function's surroundings)? A default has those only where the function runs it.
fn runs_in_body(e: &Expr) -> bool {
    struct This(bool);
    impl<'e> Visit<'e> for This {
        fn expr(&mut self, e: &'e Expr) {
            self.0 |= matches!(
                e.kind,
                ExprKind::This | ExprKind::Arrow { .. } | ExprKind::Function(_)
            );
        }
    }
    let mut v = This(false);
    walk_expr(e, &mut v);
    v.0
}
