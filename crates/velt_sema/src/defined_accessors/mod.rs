//! Accessors defined after construction (#922): `Object.defineProperty(o, "k", { get, set })`
//! on a `const o: I = { get k() { … }, set k(v) { … }, … }` of the same function, where `I` is
//! an interface that declares the accessor (an object literal implementing an interface may
//! have getters and setters, `body::expr::iface_object`). Rewritten before checking, so the
//! literal's accessors call functions held in variables that the call replaces:
//!
//! ```text
//! const o: I = { get k() { G }, set k(v) { S } };      let <o.k:get>: (() => I["k"]) | undefined = undefined;
//! …                                                    let <o.k:set>: ((v: I["k"]) => void) | undefined = undefined;
//! Object.defineProperty(o, "k", { get() { G2 } });  →  const o: I = { get k() { return <o.k:get>!(); }, set k(v) { <o.k:set>!(v); } };
//!                                                      <o.k:get> = () => { G }; <o.k:set> = (v) => { S };
//!                                                      …
//!                                                      { <o.k:get> = () => { G2 }; }
//! ```
//!
//! As in Node, an accessor the descriptor leaves out keeps the one it had, and after a call
//! with `configurable: false` defining it again throws `TypeError: Cannot redefine property:
//! k` (checked only where the property may be defined twice: another call, or a loop).
//! `enumerable` changes nothing Velt shows (an interface value is not listed by key). `this`
//! inside the accessors is not supported (#941; the parser reports it). Other accessor
//! definitions are errors in `body::expr::object_define`.

mod emit;
mod synth;

use rustc_hash::FxHashMap;
use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use crate::SourceModule;

/// Does `e` call `Object.defineProperty` with an accessor descriptor literal?
fn accessor_call(e: &ast::Expr) -> Option<(&ast::Expr, &ast::Expr, &[ast::ObjectProp])> {
    let E::Call { callee, args, .. } = &e.kind else {
        return None;
    };
    let E::Member { object, prop, .. } = &callee.kind else {
        return None;
    };
    if !matches!(&object.kind, E::Ident(o) if o.name == "Object") || prop.name != "defineProperty" {
        return None;
    }
    let [o, key, desc] = args.as_slice() else {
        return None;
    };
    let E::Object(props) = &desc.kind else {
        return None;
    };
    let accessor = props.iter().any(|p| {
        matches!(p, ast::ObjectProp::KeyValue(n, _) | ast::ObjectProp::Shorthand(n)
            if n.name == "get" || n.name == "set")
    });
    accessor.then_some((o, key, props.as_slice()))
}

/// Does `module` define an accessor with `Object.defineProperty`?
fn defines_accessors(module: &ast::Module) -> bool {
    struct Finds(bool);
    impl Visit<'_> for Finds {
        fn expr(&mut self, e: &ast::Expr) {
            self.0 |= accessor_call(e).is_some();
        }
    }
    let mut f = Finds(false);
    visit::walk_module(module, &mut f);
    f.0
}

/// Does `module` bind the name `Object` anywhere: a variable or destructured name, a
/// parameter (of a function, method or arrow), a loop or `catch` variable, a function or class,
/// or an import (a namespace too)? Its `Object.defineProperty` may then be the program's own,
/// so the module is not rewritten.
fn declares_object(module: &ast::Module) -> bool {
    struct Binds(bool);
    impl Binds {
        fn pattern(&mut self, p: &ast::Pattern) {
            let mut names = vec![];
            emit::pattern_names(p, &mut names);
            self.name(names.iter().map(String::as_str));
        }
        fn name<'n>(&mut self, mut names: impl Iterator<Item = &'n str>) {
            self.0 |= names.any(|n| n == "Object");
        }
    }
    impl Visit<'_> for Binds {
        fn item(&mut self, item: &ast::Item) {
            match &item.kind {
                I::Function(f) => self.name(std::iter::once(f.sig.name.name.as_str())),
                I::Class(t) | I::Struct(t) => self.name(std::iter::once(t.name.name.as_str())),
                I::Import(imp) => {
                    let names = imp
                        .names
                        .iter()
                        .map(|n| n.alias.as_ref().unwrap_or(&n.name));
                    self.name(names.chain(imp.namespace.iter()).map(|n| n.name.as_str()));
                }
                _ => {}
            }
        }
        fn function(&mut self, sig: &ast::FnSig, _body: &ast::Block) {
            self.name(sig.params.iter().map(|p| p.name.name.as_str()));
            sig.params
                .iter()
                .filter_map(|p| p.pattern.as_ref())
                .for_each(|p| self.pattern(p));
        }
        fn var_decl(&mut self, v: &ast::VarDecl) {
            self.pattern(&v.pattern);
        }
        fn stmt(&mut self, s: &ast::Stmt) {
            match &s.kind {
                S::ForOf { pattern, .. } => self.pattern(pattern),
                S::Try {
                    catch: Some((Some(p), _)),
                    ..
                } => self.pattern(p),
                _ => {}
            }
        }
        fn expr(&mut self, e: &ast::Expr) {
            if let E::Arrow { params, .. } = &e.kind {
                self.name(params.iter().map(|p| p.name.name.as_str()));
                params
                    .iter()
                    .filter_map(|p| p.pattern.as_ref())
                    .for_each(|p| self.pattern(p));
            }
        }
    }
    let mut b = Binds(false);
    visit::walk_module(module, &mut b);
    b.0
}

/// The rewritten modules (`None`: no module defines an accessor), and the errors found.
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Vec<Diagnostic>, Option<Vec<SourceModule>>) {
    let wanted =
        |m: &SourceModule| !m.is_std && !declares_object(&m.ast) && defines_accessors(&m.ast);
    if !modules.iter().any(wanted) {
        return (vec![], None);
    }
    let mut diags = vec![];
    let out = modules
        .iter()
        .map(|m| {
            let mut ast = m.ast.clone();
            if wanted(m) {
                let mut w = Bodies { diags: &mut diags };
                ast.items.iter_mut().for_each(|i| w.item(i));
            }
            SourceModule {
                path: m.path.clone(),
                is_std: m.is_std,
                file: m.file,
                ast,
                imports: m.imports.clone(),
                jsx_runtime: m.jsx_runtime.clone(),
            }
        })
        .collect();
    (diags, Some(out))
}

/// An accessor `defineProperty` call found in a function body.
#[derive(Clone, Debug)]
pub(super) struct Define {
    pub object: String,
    pub key: String,
    pub get: Option<ast::Expr>,
    pub set: Option<ast::Expr>,
    /// `configurable: false`, written.
    pub fixes: bool,
    /// Also `value` or `writable`, which Node rejects with an accessor.
    pub data: bool,
    pub in_loop: bool,
    pub span: Span,
}

/// A `const o: I = { … }` of a function body: the interface type and the literal's accessors.
#[derive(Clone, Debug)]
pub(super) struct Decl {
    pub ty: ast::TypeExpr,
    /// Per key: the literal has a getter, a setter.
    pub accessors: FxHashMap<String, (bool, bool)>,
}

/// Walks every function body of a module.
struct Bodies<'d> {
    diags: &'d mut Vec<Diagnostic>,
}

impl Bodies<'_> {
    fn item(&mut self, item: &mut ast::Item) {
        match &mut item.kind {
            I::Function(f) => self.func(f),
            I::Struct(t) | I::Class(t) => {
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Extend(e) => e.methods.iter_mut().for_each(|m| self.func(&mut m.decl)),
            I::Interface(i) => {
                for b in i.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    self.body(b);
                }
            }
            I::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e);
                }
            }
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i)),
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl) {
        for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(e);
        }
        self.body(&mut f.body);
    }

    /// One function body: its own rewrite, then the functions inside it.
    fn body(&mut self, b: &mut ast::Block) {
        let mut found = emit::Found::default();
        found.stmts(&b.stmts, false);
        if !found.defines.is_empty() {
            found.rewrite(&mut b.stmts, self.diags);
        }
        self.nested_stmts(&mut b.stmts);
    }

    fn nested_stmts(&mut self, stmts: &mut [ast::Stmt]) {
        for s in stmts {
            self.nested(s);
        }
    }

    /// The functions inside statement `s`.
    fn nested(&mut self, s: &mut ast::Stmt) {
        match &mut s.kind {
            S::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e);
                }
            }
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.nested_stmts(&mut then.stmts);
                if let Some(e) = els {
                    self.nested(e);
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond);
                self.nested_stmts(&mut body.stmts);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.nested(i);
                }
                cond.iter_mut()
                    .chain(update.iter_mut())
                    .for_each(|e| self.expr(e));
                self.nested_stmts(&mut body.stmts);
            }
            S::ForOf { iter, body, .. } => {
                self.expr(iter);
                self.nested_stmts(&mut body.stmts);
            }
            S::Block(b) => self.nested_stmts(&mut b.stmts),
            S::Labeled { body, .. } => self.nested(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    if let Some(t) = &mut c.test {
                        self.expr(t);
                    }
                    self.nested_stmts(&mut c.body);
                }
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.nested_stmts(&mut body.stmts);
                if let Some((_, c)) = catch {
                    self.nested_stmts(&mut c.stmts);
                }
                if let Some(f) = finally {
                    self.nested_stmts(&mut f.stmts);
                }
            }
            S::Item(item) => self.item(item),
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    /// The function bodies inside expression `e`.
    fn expr(&mut self, e: &mut ast::Expr) {
        match &mut e.kind {
            E::Arrow {
                body: ast::ArrowBody::Block(b),
                ..
            } => self.body(b),
            E::Function(d) => self.func(d),
            E::Object(props) => {
                for p in props.iter_mut() {
                    if let ast::ObjectProp::Method(d) = p {
                        self.func(d);
                    }
                }
            }
            _ => {}
        }
        crate::ast_walk::children_mut(e, &mut |c| self.expr(c));
    }
}
