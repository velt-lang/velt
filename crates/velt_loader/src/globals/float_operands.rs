//! The predicate of the conditional-global header `// load for: a conversion operand may be a
//! float` (docs/internals/design/lazy-std.md "Float operands"): whether a module converts to text
//! a value that, by its syntax alone, may be a float that is not a whole number. The types are not
//! known yet; a conversion this misses still prints correctly (std's header says what loads, and
//! the code without it takes a slower path), and one it wrongly sees only costs loading time.
//!
//! A conversion is a template literal's expression, the argument of `String(…)`, the receiver of
//! `.toString(…)`, or the other side of `+` with a string literal or a template. Its operand looks
//! like a float when it contains a float literal (`1.5`, `1e-3`), `/` or `**`, a member or call of
//! `Math`, a cast to a float type (`f64`, `f32`, `f16`, `bf16`), or a variable or parameter declared
//! with one or initialized from such an expression. An array literal looks like a float when an
//! element does, and so does a `for … of` variable over one.

use std::collections::HashSet;

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

/// Whether `module` converts to text an operand that may be a float ([module docs](self)).
pub(super) fn has_float_conversion(module: &ast::Module) -> bool {
    let names = float_names(module);
    struct Find<'n> {
        names: &'n HashSet<String>,
        found: bool,
    }
    fn texty(e: &ast::Expr) -> bool {
        matches!(
            e.kind,
            ast::ExprKind::Lit(ast::Lit::Str(_)) | ast::ExprKind::Template { .. }
        )
    }
    impl<'a> Visit<'a> for Find<'_> {
        fn expr(&mut self, e: &'a ast::Expr) {
            if self.found {
                return;
            }
            let n = self.names;
            self.found = match &e.kind {
                ast::ExprKind::Template { exprs, .. } => exprs.iter().any(|x| floaty(x, n)),
                ast::ExprKind::Call { callee, args, .. } => match &callee.kind {
                    ast::ExprKind::Ident(id) if id.name == "String" => {
                        args.first().is_some_and(|a| floaty(a, n))
                    }
                    ast::ExprKind::Member { object, prop, .. } if prop.name == "toString" => {
                        floaty(object, n)
                    }
                    _ => false,
                },
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::Add,
                    lhs,
                    rhs,
                } => (texty(lhs) && floaty(rhs, n)) || (texty(rhs) && floaty(lhs, n)),
                _ => false,
            };
        }
    }
    let mut find = Find {
        names: &names,
        found: false,
    };
    visit::walk_module(module, &mut find);
    find.found
}

/// The names of the variables and parameters `module` declares with a float type, or initializes
/// from an expression that looks like a float, in source order (scopes are not told apart: a
/// shadowing name only costs a false positive).
fn float_names(module: &ast::Module) -> HashSet<String> {
    struct Names(HashSet<String>);
    impl<'a> Visit<'a> for Names {
        fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
            for p in &sig.params {
                if is_float_type(&p.ty) {
                    self.0.insert(p.name.name.clone());
                }
            }
        }
        fn stmt(&mut self, s: &'a ast::Stmt) {
            // `for (const x of xs)` over values that look like floats.
            if let ast::StmtKind::ForOf { pattern, iter, .. } = &s.kind {
                if let ast::PatternKind::Ident(id) = &pattern.kind {
                    if floaty(iter, &self.0) {
                        self.0.insert(id.name.clone());
                    }
                }
            }
        }
        fn var_decl(&mut self, v: &'a ast::VarDecl) {
            let ast::PatternKind::Ident(id) = &v.pattern.kind else {
                return;
            };
            let typed = v.ty.as_ref().is_some_and(is_float_type);
            if typed || v.init.as_ref().is_some_and(|e| floaty(e, &self.0)) {
                self.0.insert(id.name.clone());
            }
        }
    }
    let mut names = Names(HashSet::new());
    visit::walk_module(module, &mut names);
    names.0
}

/// A float type: `f64`, `f32`, `f16` or `bf16` (also inside `T | null`).
fn is_float_type(t: &ast::TypeExpr) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } => {
            args.is_empty()
                && matches!(path.as_slice(), [one]
                    if matches!(one.name.as_str(), "f64" | "f32" | "f16" | "bf16"))
        }
        ast::TypeExprKind::Union(parts) => parts.iter().any(is_float_type),
        _ => false,
    }
}

/// Whether expression `e` looks like a float ([module docs](self)); `names` are the variables
/// known to hold one.
fn floaty(e: &ast::Expr, names: &HashSet<String>) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Float { .. }) => true,
        ast::ExprKind::Ident(id) => names.contains(&id.name),
        ast::ExprKind::Binary { op, lhs, rhs } => {
            matches!(op, ast::BinaryOp::Div | ast::BinaryOp::Pow)
                || floaty(lhs, names)
                || floaty(rhs, names)
        }
        ast::ExprKind::Unary { expr, .. }
        | ast::ExprKind::Paren(expr)
        | ast::ExprKind::NonNull(expr)
        | ast::ExprKind::Unsafe(expr)
        | ast::ExprKind::Await(expr) => floaty(expr, names),
        ast::ExprKind::Cond { then, els, .. } => floaty(then, names) || floaty(els, names),
        // An array holding one (`const xs = [1.5, 2]`, iterated by `for … of`).
        ast::ExprKind::Array(elems) => elems.iter().any(|x| floaty(x, names)),
        ast::ExprKind::Cast { expr, ty } => is_float_type(ty) || floaty(expr, names),
        ast::ExprKind::Member { object, .. } => is_math(object),
        ast::ExprKind::Call { callee, .. } => match &callee.kind {
            ast::ExprKind::Member { object, .. } => is_math(object),
            _ => false,
        },
        _ => false,
    }
}

/// `Math` (its members and methods give floats).
fn is_math(e: &ast::Expr) -> bool {
    matches!(&e.kind, ast::ExprKind::Ident(id) if id.name == "Math")
}
