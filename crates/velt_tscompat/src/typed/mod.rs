//! The rules that need types or what a name refers to: they ask the checker's IDE analysis
//! ([`velt_sema::ide::Analysis`]: `type_of`, `view`, `fields`, `def_of`) about the nodes one
//! walk over each module ([`velt_syntax::visit`]) hands them.
//!
//! - [`numbers`] for `int-division`, `default-sort` (with the call arguments [`args`] collects);
//! - [`templates`] for `object-in-template`, `json-map`;
//! - [`strings`] for `map-iter-as-array`;
//! - [`globals`] for `velt-global`, `velt-member`, with the prelude's classification in
//!   [`prelude`];
//! - [`catch`] for `catch-unknown`.

mod args;
mod catch;
mod globals;
mod numbers;
mod prelude;
mod returns;
mod strings;
mod templates;

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_sema::ide::{DefRef, TypeRef, TypeView};
use velt_syntax::ast::{self, ExprKind as E};
use velt_syntax::visit::{self, Visit};

use crate::findings::Cx;
use crate::{Finding, LintModule, Program};

/// What the typed rules need to know about declarations anywhere in the program.
pub(crate) struct Decls {
    /// The parameters (declaring identifiers) of each function, method and class constructor,
    /// by the declaring identifier of the function, method or class.
    params: HashMap<Span, Vec<Span>>,
}

impl Decls {
    pub(crate) fn collect(program: &Program) -> Decls {
        let mut collect = CollectDecls {
            decls: Decls {
                params: HashMap::new(),
            },
        };
        for m in program.modules.iter().filter(|m| !m.is_std) {
            visit::walk_module(&m.ast, &mut collect);
        }
        collect.decls
    }
}

struct CollectDecls {
    decls: Decls,
}

impl CollectDecls {
    fn sig(&mut self, owner: Span, sig: &ast::FnSig) {
        let params = sig.params.iter().map(|p| p.name.span).collect();
        self.decls.params.insert(owner, params);
    }
}

impl<'a> Visit<'a> for CollectDecls {
    fn item(&mut self, item: &'a ast::Item) {
        match &item.kind {
            ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) => {
                if let Some(ctor) = &t.constructor {
                    self.sig(t.name.span, &ctor.sig);
                }
            }
            ast::ItemKind::Interface(i) => {
                for m in i.methods.iter().filter(|m| m.body.is_none()) {
                    self.sig(m.sig.name.span, &m.sig);
                }
            }
            ast::ItemKind::ExternFn(sig) => self.sig(sig.name.span, sig),
            _ => {}
        }
    }

    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        self.sig(sig.name.span, sig);
    }
}

/// Every typed finding in `module`.
pub(crate) fn lint_module(module: &LintModule, program: &Program, decls: &Decls) -> Vec<Finding> {
    let mut walk = Walk {
        t: Typed {
            cx: Cx::new(module.src),
            program,
            decls,
            truncated: HashSet::new(),
            int_positions: HashSet::new(),
            catch: catch::State::default(),
        },
    };
    visit::walk_module(module.ast, &mut walk);
    walk.t.cx.findings
}

/// What the typed rules share while walking one module.
pub(crate) struct Typed<'a> {
    pub(crate) cx: Cx<'a>,
    program: &'a Program<'a>,
    decls: &'a Decls,
    /// `a / b` directly inside `Math.trunc(…)`, which truncates in both languages.
    truncated: HashSet<(u32, u32)>,
    /// Expressions that go where an integer is declared ([`numbers::int_position`]).
    int_positions: HashSet<(u32, u32)>,
    catch: catch::State,
}

impl Typed<'_> {
    /// The type the checker recorded for `e`.
    fn type_of(&self, e: &ast::Expr) -> Option<TypeRef> {
        self.program.analysis.type_of(e.span)
    }

    /// The structure of `e`'s type ([`TypeView::Other`] when none was recorded).
    fn view_of(&self, e: &ast::Expr) -> TypeView {
        match self.type_of(e) {
            Some(t) => self.program.analysis.view(&t),
            None => TypeView::Other,
        }
    }

    /// The structure of the type of an assignment's target: a variable's is its declaration's
    /// (an assigned place isn't recorded as an expression).
    fn place_view(&self, target: &ast::Expr) -> TypeView {
        if let Some(t) = self.type_of(target) {
            return self.view(&t);
        }
        let E::Ident(id) = &target.kind else {
            return TypeView::Other;
        };
        match self
            .def(id.span)
            .and_then(|d| self.program.analysis.type_of(d.span))
        {
            Some(t) => self.view(&t),
            None => TypeView::Other,
        }
    }

    fn view(&self, t: &TypeRef) -> TypeView {
        self.program.analysis.view(t)
    }

    /// The definition the name at `span` refers to.
    fn def(&self, span: Span) -> Option<DefRef> {
        self.program.analysis.def_of(span)
    }

    /// Whether `d` is declared in the standard library.
    fn is_std(&self, d: &DefRef) -> bool {
        self.program.modules.get(d.module).is_some_and(|m| m.is_std)
    }
}

/// The visitor: hands nodes to the rules.
struct Walk<'a> {
    t: Typed<'a>,
}

impl<'a> Walk<'a> {
    fn declared_returns(&mut self, ret: &ast::TypeExpr, values: Vec<&'a ast::Expr>) {
        let int = numbers::int_type_expr(ret);
        for e in &values {
            strings::iter_as_array(e, &mut self.t);
            if int {
                numbers::int_position(e, &mut self.t);
            }
        }
    }
}

impl<'a> Visit<'a> for Walk<'a> {
    fn function(&mut self, sig: &'a ast::FnSig, body: &'a ast::Block) {
        if let Some(ret) = &sig.ret {
            self.declared_returns(ret, returns::returned(body));
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        catch::stmt(s, &mut self.t);
        // `walk_module` leaves the defaults in patterns out.
        match &s.kind {
            ast::StmtKind::ForOf { pattern, .. }
            | ast::StmtKind::Try {
                catch: Some((Some(pattern), _)),
                ..
            } => visit::walk_pattern(pattern, self),
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        if let (Some(ty), Some(init)) = (&v.ty, &v.init) {
            strings::iter_as_array(init, &mut self.t);
            if numbers::int_type_expr(ty) || numbers::int_local(&v.pattern, &self.t) {
                numbers::int_position(init, &mut self.t);
            }
        }
        visit::walk_pattern(&v.pattern, self);
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        let t = &mut self.t;
        match &e.kind {
            E::Binary { op, lhs, rhs } => {
                numbers::operand_positions(*op, lhs, rhs, &mut self.t);
                numbers::binary(e, *op, lhs, rhs, &mut self.t);
                catch::binary(*op, lhs, rhs, &mut self.t);
            }
            E::Assign { op, target, value } => {
                if numbers::int_view(&t.place_view(target), t) {
                    numbers::int_position(value, t);
                }
                numbers::assign(e, *op, target, value, t);
            }
            E::Template { exprs, .. } => templates::template(exprs, t),
            E::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                numbers::call(e, callee, args, t);
                templates::call(callee, args, t);
                args::call(callee, args, t);
                globals::call(callee, type_args, t);
            }
            E::Member { object, prop, .. } => {
                strings::member(object, t);
                globals::member(e, object, prop, t);
                catch::member(object, t);
            }
            E::Index { object, .. } => {
                strings::index(object, t);
                catch::member(object, t);
            }
            E::New { class, args } => args::new(class, args, t),
            E::Object(props) => numbers::object(e, props, t),
            E::Ident(id) => globals::ident(id, t),
            E::Cond { cond, then, .. } => catch::cond(cond, then, t),
            E::Arrow {
                ret: Some(ret),
                body,
                ..
            } => self.declared_returns(ret, returns::arrow_returned(body)),
            _ => {}
        }
        if let E::Arrow { params, .. } = &e.kind {
            // `walk_module` leaves arrow parameter defaults out.
            for default in params.iter().filter_map(|p| p.default.as_ref()) {
                visit::walk_expr(default, self);
            }
        }
    }

    fn ty(&mut self, ty: &'a ast::TypeExpr) {
        globals::ty(ty, &mut self.t);
    }
}
