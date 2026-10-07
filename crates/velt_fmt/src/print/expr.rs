//! Expression dispatch and the simple forms (unary, update, ternary, assignment, casts,
//! parentheses). Operators live in `binary`, calls in `call`/`chain`, literals in `literals`.
//! Parentheses are printed exactly where the AST has `Paren` nodes, so precedence is unchanged.

use velt_syntax::ast::{BinaryOp, Expr, ExprKind, UnaryOp, UpdateOp};

use super::call::member_suffix;
use super::func::breaks_itself;
use super::Printer;
use crate::doc::{cat, group, indent, line, softline, text, Doc};
use crate::source::slice;

impl<'a> Printer<'a> {
    /// An expression, preceded by the comments before it.
    pub(super) fn expr(&mut self, e: &Expr) -> Doc {
        self.with_leading(e.span.lo, |p| p.expr_kind(e))
    }

    /// Like [`Printer::expr`], but a binary chain is not indented after its first operand (the
    /// context already indents or brackets it: conditions, assignment right-hand sides...).
    pub(super) fn expr_no_indent(&mut self, e: &Expr) -> Doc {
        match &e.kind {
            ExprKind::Binary { .. } => self.with_leading(e.span.lo, |p| p.binary(e, false)),
            _ => self.expr(e),
        }
    }

    fn expr_kind(&mut self, e: &Expr) -> Doc {
        match &e.kind {
            ExprKind::Lit(lit) => self.lit(lit, e.span),
            ExprKind::Template { .. } => {
                self.comments.skip_until(e.span.hi);
                text(slice(self.src, e.span))
            }
            ExprKind::Ident(id) => text(id.name.clone()),
            ExprKind::This => "this".into(),
            ExprKind::Super => "super".into(),
            ExprKind::Unary { op, expr } => self.unary(*op, expr),
            ExprKind::Binary { .. } => self.binary(e, true),
            ExprKind::Assign { op, target, value } => {
                let op = match op {
                    Some(op) => format!(" {}=", binary_op(*op)),
                    None => " =".to_string(),
                };
                let target = self.expr(target);
                self.assignment(target, &op, value)
            }
            ExprKind::Update { op, prefix, target } => {
                let op = update_op(*op);
                let target = self.expr(target);
                if *prefix {
                    cat![op, target]
                } else {
                    cat![target, op]
                }
            }
            ExprKind::Cond { cond, then, els } => self.conditional(cond, then, els),
            ExprKind::Call {
                callee,
                type_args,
                args,
                optional,
            } => self.call(e, callee, type_args, args, *optional),
            // A regular expression literal (parsed as `new RegExp(…)`) is kept as written.
            ExprKind::New { .. } if crate::source::slice(self.src, e.span).starts_with('/') => {
                text(crate::source::slice(self.src, e.span).to_string())
            }
            ExprKind::New { class, args } => {
                let class = self.ty(class);
                cat!["new ", class, self.args(args, e.span.hi)]
            }
            ExprKind::Member {
                object,
                prop,
                optional,
            } => {
                let object = self.expr(object);
                cat![object, member_suffix(prop, *optional)]
            }
            ExprKind::Index {
                object,
                index,
                optional,
            } => {
                let object = self.expr(object);
                cat![object, self.index_suffix(index, *optional)]
            }
            ExprKind::Arrow {
                type_params,
                params,
                ret,
                throws,
                body,
                is_async,
            } => self.arrow(
                type_params,
                params,
                (ret.as_ref(), throws.as_ref()),
                body,
                *is_async,
            ),
            ExprKind::Function(f) => self.fn_decl(f, "function "),
            ExprKind::Array(elems) => self.array(elems, e.span.hi),
            ExprKind::Object(props) => self.object(props, e.span.hi),
            ExprKind::StructLit { name, props } => self.struct_lit(name, props, e.span.hi),
            ExprKind::Spread(inner) => cat!["...", self.expr(inner)],
            ExprKind::Await(inner) => cat!["await ", self.expr(inner)],
            ExprKind::Yield { arg, delegate } => {
                let kw = if *delegate { "yield*" } else { "yield" };
                match arg {
                    Some(a) => cat![kw, " ", self.expr(a)],
                    None => text(kw),
                }
            }
            ExprKind::Cast { expr, ty } => {
                let inner = self.expr(expr);
                cat![inner, " as ", self.ty_cast(ty)]
            }
            ExprKind::InstanceOf { expr, ty } => {
                let inner = self.expr(expr);
                cat![inner, " instanceof ", self.ty(ty)]
            }
            ExprKind::Paren(inner) => self.paren(inner),
            ExprKind::NonNull(inner) => cat![self.expr(inner), "!"],
            ExprKind::Jsx(element) => self.jsx_element(element),
        }
    }

    fn unary(&mut self, op: UnaryOp, inner: &Expr) -> Doc {
        let (op, sign) = match op {
            UnaryOp::Neg => ("-", Some('-')),
            UnaryOp::Plus => ("+", Some('+')),
            UnaryOp::Not => ("!", None),
            UnaryOp::BitNot => ("~", None),
            UnaryOp::TypeOf => ("typeof ", None),
            UnaryOp::Delete => ("delete ", None),
        };
        // `- -x` must not become the decrement `--x`.
        let space = if sign.is_some() && leading_sign(inner) == sign {
            " "
        } else {
            ""
        };
        cat![op, space, self.expr(inner)]
    }

    fn conditional(&mut self, cond: &Expr, then: &Expr, els: &Expr) -> Doc {
        if super::jsx::is_jsx_conditional(cond, then, els) {
            return self.jsx_conditional(cond, then, els);
        }
        let cond = self.expr(cond);
        let then = self.expr(then);
        let els = self.expr(els);
        group(cat![
            cond,
            indent(cat![line(), "? ", then, line(), ": ", els])
        ])
    }

    fn paren(&mut self, inner: &Expr) -> Doc {
        match inner.kind {
            ExprKind::Binary { .. } | ExprKind::Cond { .. } | ExprKind::Assign { .. } => {
                let inner = self.expr_no_indent(inner);
                group(cat!["(", indent(cat![softline(), inner]), softline(), ")"])
            }
            _ => cat!["(", self.expr(inner), ")"],
        }
    }

    /// `lhs op value` (assignments, initializers, defaults, properties; `op` carries its own
    /// leading space, e.g. `" ="` or `":"`): the value starts on the same line when it breaks
    /// well by itself, else it moves to an indented next line when too long.
    pub(super) fn assignment(&mut self, lhs: Doc, op: &str, value: &Expr) -> Doc {
        if super::jsx::is_jsx_layout(value) {
            let value = self.expr_jsx_parens(value);
            return group(cat![lhs, op.to_string(), " ", value]);
        }
        if breaks_itself(value) {
            let value = self.expr(value);
            return group(cat![lhs, op.to_string(), " ", value]);
        }
        let value = self.expr_no_indent(value);
        group(cat![
            lhs,
            op.to_string(),
            group(indent(cat![line(), value]))
        ])
    }
}

/// The sign a printed expression starts with, if any (follows the leftmost operand).
fn leading_sign(mut e: &Expr) -> Option<char> {
    loop {
        e = match &e.kind {
            ExprKind::Unary {
                op: UnaryOp::Neg, ..
            } => return Some('-'),
            ExprKind::Unary {
                op: UnaryOp::Plus, ..
            } => return Some('+'),
            ExprKind::Update {
                op, prefix: true, ..
            } => return Some(if *op == UpdateOp::Dec { '-' } else { '+' }),
            ExprKind::Binary { lhs: first, .. }
            | ExprKind::Assign { target: first, .. }
            | ExprKind::Cond { cond: first, .. }
            | ExprKind::Call { callee: first, .. }
            | ExprKind::Member { object: first, .. }
            | ExprKind::Index { object: first, .. }
            | ExprKind::Cast { expr: first, .. }
            | ExprKind::InstanceOf { expr: first, .. }
            | ExprKind::Update { target: first, .. } => first,
            _ => return None,
        };
    }
}

fn update_op(op: UpdateOp) -> &'static str {
    match op {
        UpdateOp::Inc => "++",
        UpdateOp::Dec => "--",
    }
}

/// Spelling of a binary operator (`==`/`!=`; `binary` restores `===`/`!==` from the source).
pub(super) fn binary_op(op: BinaryOp) -> &'static str {
    use BinaryOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Pow => "**",
        Eq => "==",
        NotEq => "!=",
        Lt => "<",
        LtEq => "<=",
        Gt => ">",
        GtEq => ">=",
        And => "&&",
        Or => "||",
        Nullish => "??",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Shl => "<<",
        Shr => ">>",
        UShr => ">>>",
        In => "in",
    }
}
