//! The message of the replaced-holder error: the operator and right-hand side as written, and
//! the fix.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::Candidate;
use crate::ctx::Ctx;

/// The text of an assignment operator (`+=`).
fn op_text(op: Option<ast::BinaryOp>) -> &'static str {
    use ast::BinaryOp as B;
    match op {
        Some(B::Add) => "+=",
        Some(B::Sub) => "-=",
        Some(B::Mul) => "*=",
        Some(B::Div) => "/=",
        Some(B::Rem) => "%=",
        Some(B::Pow) => "**=",
        Some(B::BitAnd) => "&=",
        Some(B::BitOr) => "|=",
        Some(B::BitXor) => "^=",
        Some(B::Shl) => "<<=",
        Some(B::Shr) => ">>=",
        Some(B::UShr) => ">>>=",
        Some(B::And) => "&&=",
        Some(B::Or) => "||=",
        Some(B::Nullish) => "??=",
        _ => "=",
    }
}

/// The assignment written at `span`: its operator and right-hand side.
fn written<'m>(cx: &Ctx<'m>, span: Span) -> Option<(Option<ast::BinaryOp>, &'m ast::Expr)> {
    struct Find<'a> {
        span: Span,
        found: Option<(Option<ast::BinaryOp>, &'a ast::Expr)>,
    }
    impl<'a> velt_syntax::visit::Visit<'a> for Find<'a> {
        fn expr(&mut self, e: &'a ast::Expr) {
            if let ast::ExprKind::Assign { op, value, .. } = &e.kind {
                if e.span == self.span {
                    self.found = Some((*op, value));
                }
            }
        }
    }
    let m = cx.modules.iter().find(|m| m.file == span.file)?;
    let mut f = Find { span, found: None };
    velt_syntax::visit::walk_module(&m.ast, &mut f);
    f.found
}

pub(super) fn report(cx: &mut Ctx, c: &Candidate) {
    let holder = &c.holder_text;
    let target = &c.target_text;
    let (op, rhs, value_span) = match written(cx, c.span) {
        Some((op, value)) => (op, source(value, 0), value.span),
        None => (None, None, c.value_span),
    };
    let op_s = op_text(op);
    let compute = match &rhs {
        Some(rhs) => format!("compute the value first: `const v = {rhs}; "),
        None => "compute the right-hand side into a variable `v` first: `".to_string(),
    };
    // A logical assignment evaluates its right-hand side only when it assigns.
    let fix = match op {
        Some(ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish) => {
            let cond = match op {
                Some(ast::BinaryOp::And) => target.to_string(),
                Some(ast::BinaryOp::Or) => format!("!{target}"),
                _ => format!("{target} == null"),
            };
            let (lead, inner) = compute.split_once('`').unwrap_or((&compute, ""));
            format!("{lead}`if ({cond}) {{ {inner}{target} = v; }}`")
        }
        _ => format!("{compute}{target} {op_s} v;`"),
    };
    let d = Diagnostic::error(
        format!("the right-hand side may replace `{holder}`, the object `{target} {op_s} …` writes into"),
        c.span,
    )
    .with_label(value_span, format!("this may assign a new object to `{holder}`"))
    .with_note(format!(
        "JavaScript picks the object before the right-hand side runs and writes into the old one, which nothing refers to any more; `{holder}` is an object literal stored inside `{parent}`, so Velt would write into the new one",
        parent = c.parent_text
    ))
    .with_note(fix);
    cx.error(d);
}

/// The source of a short expression (calls, member paths, operators, literals), for the fix;
/// `None` for a longer one or one of another kind.
fn source(e: &ast::Expr, depth: u32) -> Option<String> {
    use ast::BinaryOp as B;
    use ast::ExprKind as A;
    if depth > 12 {
        return None;
    }
    let s = |x: &ast::Expr| source(x, depth + 1);
    let list = |xs: &[ast::Expr]| -> Option<String> {
        Some(xs.iter().map(s).collect::<Option<Vec<_>>>()?.join(", "))
    };
    let q = |optional: bool| if optional { "?." } else { "" };
    let out = match &e.kind {
        A::Ident(id) => id.name.clone(),
        A::This => "this".into(),
        A::Paren(x) => format!("({})", s(x)?),
        A::NonNull(x) => format!("{}!", s(x)?),
        A::Await(x) => format!("await {}", s(x)?),
        A::Member {
            object,
            prop,
            optional,
        } => format!(
            "{}{}{}",
            s(object)?,
            if *optional { "?." } else { "." },
            prop.name
        ),
        A::Index {
            object,
            index,
            optional,
        } => format!("{}{}[{}]", s(object)?, q(*optional), s(index)?),
        A::Call {
            callee,
            type_args,
            args,
            optional,
        } if type_args.is_empty() => {
            format!("{}{}({})", s(callee)?, q(*optional), list(args)?)
        }
        A::New { class, args } => match &class.kind {
            ast::TypeExprKind::Named { path, args: targs } if targs.is_empty() => {
                let name: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
                format!("new {}({})", name.join("."), list(args)?)
            }
            _ => return None,
        },
        A::Unary { op, expr } => {
            let op = match op {
                ast::UnaryOp::Neg => "-",
                ast::UnaryOp::Plus => "+",
                ast::UnaryOp::Not => "!",
                ast::UnaryOp::BitNot => "~",
                ast::UnaryOp::TypeOf => "typeof ",
                ast::UnaryOp::Delete => "delete ",
            };
            format!("{op}{}", s(expr)?)
        }
        A::Binary { op, lhs, rhs } => {
            let op = match op {
                B::Add => "+",
                B::Sub => "-",
                B::Mul => "*",
                B::Div => "/",
                B::Rem => "%",
                B::Pow => "**",
                B::Eq => "===",
                B::NotEq => "!==",
                B::Lt => "<",
                B::LtEq => "<=",
                B::Gt => ">",
                B::GtEq => ">=",
                B::And => "&&",
                B::Or => "||",
                B::Nullish => "??",
                B::BitAnd => "&",
                B::BitOr => "|",
                B::BitXor => "^",
                B::Shl => "<<",
                B::Shr => ">>",
                B::UShr => ">>>",
                B::In => return None,
            };
            format!("{} {op} {}", s(lhs)?, s(rhs)?)
        }
        A::Lit(ast::Lit::Int { value, suffix }) => {
            format!("{value}{}", suffix.as_deref().unwrap_or(""))
        }
        A::Lit(ast::Lit::Float { value, suffix }) => {
            format!("{value}{}", suffix.as_deref().unwrap_or(""))
        }
        A::Lit(ast::Lit::Str(v)) => format!("{v:?}"),
        A::Lit(ast::Lit::Bool(b)) => b.to_string(),
        A::Lit(ast::Lit::Null) => "null".into(),
        A::Template { quasis, exprs } => {
            let mut out = String::from("`");
            for (i, part) in quasis.iter().enumerate() {
                if part.contains(['`', '$', '\\']) {
                    return None;
                }
                out.push_str(part);
                if let Some(x) = exprs.get(i) {
                    out.push_str(&format!("${{{}}}", s(x)?));
                }
            }
            out.push('`');
            out
        }
        _ => return None,
    };
    (out.chars().count() <= 80).then_some(out)
}
