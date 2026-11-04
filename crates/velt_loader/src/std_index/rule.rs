//! The (a1) rule and the `extend` members, read off a whole module: every `class` and `extend`
//! wherever it is declared (a class in a function body changes its base's subclass closure as a
//! top-level one does), and every interface declared twice in one scope.
//!
//! A name is looked up as the checker scopes it: the innermost block that declares it, else the
//! module's top level. What a name must resolve to:
//! - `extends`: a class declared in this module. A type alias is not one: `type B = Base;` names
//!   whatever it is written with.
//! - `extend`: a class, struct, interface or enum declared in this module, or an interface of
//!   one of its namespaces (`N.I`; a namespace holds only interfaces, aliases and namespaces),
//!   and not an alias either (`export type i8x16 = i8x16;` re-exports a built-in vector type).

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_syntax::ast;
use velt_syntax::visit::{walk_expr, walk_module, walk_pattern, Visit};

use super::{ExtendMember, Violation};

/// What a type name declared in a scope is, as far as the rule cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Declared {
    Class,
    /// A struct, an interface or an enum.
    OtherType,
    Alias,
}

type Scope<'a> = HashMap<&'a str, Declared>;

/// Adds to `extends` and `violations` what the rule sees in `module`.
pub(super) fn check(
    module: &ast::Module,
    extends: &mut Vec<ExtendMember>,
    violations: &mut Vec<Violation>,
) {
    let mut interfaces = HashSet::new();
    merged_interfaces(&module.items, "", &mut interfaces, violations);
    let mut rule = Rule {
        top: declared(module.items.iter()),
        interfaces,
        scopes: vec![],
        extends,
        violations,
    };
    walk_module(module, &mut rule);
}

/// Adds to `seen` each interface of `items` and the namespaces in them, with its namespace path
/// (`N.I`), and reports the ones declared twice. A namespace's blocks are one scope: `namespace
/// N` can be written twice, and `namespace A.B` is `A` holding `B`.
fn merged_interfaces(
    items: &[ast::Item],
    path: &str,
    seen: &mut HashSet<String>,
    violations: &mut Vec<Violation>,
) {
    for item in items {
        match &item.kind {
            ast::ItemKind::Interface(i) => {
                let name = format!("{path}{}", i.name.name);
                if !seen.insert(name.clone()) {
                    push_once(violations, Violation::MergesInterface { name });
                }
            }
            ast::ItemKind::Namespace(n) => {
                let path = format!("{path}{}.", n.name.name);
                merged_interfaces(&n.items, &path, seen, violations);
            }
            _ => {}
        }
    }
}

/// The type names `items` declare.
fn declared<'a>(items: impl Iterator<Item = &'a ast::Item>) -> Scope<'a> {
    items
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Class(t) => Some((t.name.name.as_str(), Declared::Class)),
            ast::ItemKind::Struct(t) => Some((t.name.name.as_str(), Declared::OtherType)),
            ast::ItemKind::Interface(i) => Some((i.name.name.as_str(), Declared::OtherType)),
            ast::ItemKind::Enum(e) => Some((e.name.name.as_str(), Declared::OtherType)),
            ast::ItemKind::TypeAlias(t) => Some((t.name.name.as_str(), Declared::Alias)),
            _ => None,
        })
        .collect()
}

struct Rule<'a, 'o> {
    /// The module's top level.
    top: Scope<'a>,
    /// The interfaces of the top level and the namespaces, with their paths.
    interfaces: HashSet<String>,
    /// The blocks that declare types, each with where it is, entered before anything in it is
    /// visited: of two that hold one place, the later one is inside the earlier one.
    scopes: Vec<(Span, Scope<'a>)>,
    extends: &'o mut Vec<ExtendMember>,
    violations: &'o mut Vec<Violation>,
}

impl<'a> Rule<'a, '_> {
    /// What `name`, written at `at`, resolves to in this module.
    fn lookup(&self, name: &str, at: Span) -> Option<Declared> {
        self.scopes
            .iter()
            .rev()
            .filter(|(span, _)| span.lo <= at.lo && at.hi <= span.hi)
            .find_map(|(_, scope)| scope.get(name).copied())
            .or_else(|| self.top.get(name).copied())
    }

    /// Enters the block of `stmts` at `span`: its declarations are in scope in all of it.
    fn enter(&mut self, span: Span, stmts: impl Iterator<Item = &'a ast::Stmt> + Clone) {
        let items = || {
            stmts.clone().filter_map(|s| match &s.kind {
                ast::StmtKind::Item(item) => Some(&**item),
                _ => None,
            })
        };
        let mut interfaces = HashSet::new();
        for item in items() {
            if let ast::ItemKind::Interface(i) = &item.kind {
                if !interfaces.insert(i.name.name.as_str()) {
                    let name = i.name.name.clone();
                    push_once(self.violations, Violation::MergesInterface { name });
                }
            }
        }
        let scope = declared(items());
        if !scope.is_empty() {
            self.scopes.push((span, scope));
        }
    }

    fn enter_block(&mut self, b: &'a ast::Block) {
        self.enter(b.span, b.stmts.iter());
    }

    fn opt_pattern(&mut self, p: Option<&'a ast::Pattern>) {
        if let Some(p) = p {
            walk_pattern(p, self);
        }
    }
}

impl<'a> Visit<'a> for Rule<'a, '_> {
    fn item(&mut self, item: &'a ast::Item) {
        match &item.kind {
            ast::ItemKind::Function(f) if velt_syntax::is_script_main(&f.sig.name) => {
                self.violations.push(Violation::TopLevelStatements);
            }
            ast::ItemKind::Extend(e) => {
                let ty = extended_type(e);
                self.extends.extend(e.methods.iter().map(|m| ExtendMember {
                    ty: ty.clone(),
                    member: m.decl.sig.name.name.clone(),
                    is_static: m.is_static,
                }));
                let own = self.interfaces.contains(&ty)
                    || matches!(
                        self.lookup(&ty, e.target.span),
                        Some(Declared::Class | Declared::OtherType)
                    );
                if !own {
                    push_once(self.violations, Violation::ExtendsForeignType { ty });
                }
            }
            ast::ItemKind::Class(t) => {
                if let Some(base) = &t.extends {
                    let base = type_name(base);
                    if self.lookup(&base, t.name.span) != Some(Declared::Class) {
                        let class = t.name.name.clone();
                        let v = Violation::SubclassesForeignClass { class, base };
                        self.violations.push(v);
                    }
                }
            }
            ast::ItemKind::Augment(a) => {
                self.violations.push(match &a.module {
                    Some((spec, _)) => Violation::AugmentsModule { spec: spec.clone() },
                    None => Violation::AugmentsGlobal,
                });
            }
            _ => {}
        }
    }

    fn function(&mut self, sig: &'a ast::FnSig, body: &'a ast::Block) {
        self.enter_block(body);
        for p in &sig.params {
            self.opt_pattern(p.pattern.as_ref());
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        use ast::StmtKind as S;
        match &s.kind {
            S::If { then, .. } => self.enter_block(then),
            S::While { body, .. }
            | S::DoWhile { body, .. }
            | S::For { body, .. }
            | S::Block(body)
            | S::Unsafe(body) => self.enter_block(body),
            S::ForOf { pattern, body, .. } => {
                self.enter_block(body);
                walk_pattern(pattern, self);
            }
            S::Switch { cases, .. } => {
                // The cases of a `switch` are one block.
                self.enter(s.span, cases.iter().flat_map(|c| &c.body));
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.enter_block(body);
                if let Some((pattern, block)) = catch {
                    self.enter_block(block);
                    self.opt_pattern(pattern.as_ref());
                }
                if let Some(block) = finally {
                    self.enter_block(block);
                }
            }
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        walk_pattern(&v.pattern, self);
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        if let ast::ExprKind::Arrow { params, body, .. } = &e.kind {
            if let ast::ArrowBody::Block(b) = body {
                self.enter_block(b);
            }
            // The walk leaves out an arrow's parameter defaults.
            for p in params {
                if let Some(default) = &p.default {
                    walk_expr(default, self);
                }
                self.opt_pattern(p.pattern.as_ref());
            }
        }
    }
}

fn push_once(violations: &mut Vec<Violation>, v: Violation) {
    if !violations.contains(&v) {
        violations.push(v);
    }
}

/// [`ExtendMember::ty`] of `e`'s target.
fn extended_type(e: &ast::ExtendDecl) -> String {
    let name = type_name(&e.target);
    match e.generics.iter().any(|g| g.name.name == name) {
        true => "*".into(),
        false => name,
    }
}

/// The name `t` is written with, without its arguments: `Array` for `T[]`, `*` for a type with
/// no name.
fn type_name(t: &ast::TypeExpr) -> String {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } => path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        ast::TypeExprKind::Array(_) => "Array".into(),
        _ => "*".into(),
    }
}
