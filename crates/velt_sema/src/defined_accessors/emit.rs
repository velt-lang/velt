//! Finding the accessor definitions of one function body and rewriting them (see the parent
//! module).

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};

use super::synth::{
    assign, expr, get_forward, getter_type, ident, ident_pattern, let_cell, member_type,
    named_type, set_forward, setter_type, stmt, typed_setter,
};
use super::{accessor_call, Decl, Define};

/// The `const` object literals and accessor definitions of one function body (not of the
/// functions inside it).
#[derive(Default)]
pub(super) struct Found {
    decls: FxHashMap<String, Decl>,
    /// Names declared more than once in the body: which one a call means is not followed.
    repeated: FxHashSet<String>,
    pub defines: Vec<Define>,
}

/// What the rewrite does to one property: its variables, and whether it is checked for
/// being defined again.
struct Plan {
    ty: ast::TypeExpr,
    setter: bool,
    fixed: bool,
}

impl Found {
    /// Record the statements of a block (`in_loop`: inside a loop of the body).
    pub(super) fn stmts(&mut self, stmts: &[ast::Stmt], in_loop: bool) {
        for s in stmts {
            self.stmt(s, in_loop);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt, in_loop: bool) {
        match &s.kind {
            S::Var(v) => self.decl(v),
            S::Expr(e) => self.define(e, in_loop),
            S::If { then, els, .. } => {
                self.stmts(&then.stmts, in_loop);
                if let Some(e) = els {
                    self.stmt(e, in_loop);
                }
            }
            S::While { body, .. } | S::DoWhile { body, .. } | S::ForOf { body, .. } => {
                self.stmts(&body.stmts, true)
            }
            S::For { init, body, .. } => {
                if let Some(i) = init {
                    self.stmt(i, in_loop);
                }
                self.stmts(&body.stmts, true);
            }
            S::Block(b) => self.stmts(&b.stmts, in_loop),
            S::Labeled { body, .. } => self.stmt(body, in_loop),
            S::Switch { cases, .. } => cases.iter().for_each(|c| self.stmts(&c.body, in_loop)),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.stmts(&body.stmts, in_loop);
                if let Some((_, c)) = catch {
                    self.stmts(&c.stmts, in_loop);
                }
                if let Some(f) = finally {
                    self.stmts(&f.stmts, in_loop);
                }
            }
            S::Throw(_) | S::Return(_) | S::Break(_) | S::Continue(_) | S::Item(_) | S::Empty => {}
        }
    }

    /// `const o: I = { … }` or `const o = { … } as I`.
    fn decl(&mut self, v: &ast::VarDecl) {
        let ast::PatternKind::Ident(name) = &v.pattern.kind else {
            return;
        };
        if self.decls.contains_key(&name.name) {
            self.repeated.insert(name.name.clone());
        }
        let Some((props, ty)) = literal_of(v) else {
            self.decls.remove(&name.name);
            return;
        };
        if v.kind != ast::VarKind::Const {
            return;
        }
        let mut accessors: FxHashMap<String, (bool, bool)> = FxHashMap::default();
        for p in props {
            if let ast::ObjectProp::KeyValue(k, value) = p {
                let entry = accessors.entry(k.name.clone()).or_default();
                match accessor_fn(value) {
                    Some((true, _)) => entry.0 = true,
                    Some((false, _)) => entry.1 = true,
                    None => {}
                }
            }
        }
        self.decls.insert(name.name.clone(), Decl { ty, accessors });
    }

    fn define(&mut self, e: &ast::Expr, in_loop: bool) {
        let Some((o, key, props)) = accessor_call(e) else {
            return;
        };
        let (E::Ident(o), E::Lit(ast::Lit::Str(key))) = (&o.kind, &key.kind) else {
            return;
        };
        let mut d = Define {
            object: o.name.clone(),
            key: key.clone(),
            get: None,
            set: None,
            fixes: false,
            data: false,
            in_loop,
            span: e.span,
        };
        for p in props {
            let ast::ObjectProp::KeyValue(n, v) = p else {
                continue;
            };
            match n.name.as_str() {
                "get" => d.get = Some(v.clone()),
                "set" => d.set = Some(v.clone()),
                "configurable" => d.fixes = matches!(v.kind, E::Lit(ast::Lit::Bool(false))),
                "value" | "writable" => d.data = true,
                _ => {}
            }
        }
        self.defines.push(d);
    }

    /// Rewrite the body's statements; report the definitions it can't follow.
    pub(super) fn rewrite(&self, stmts: &mut Vec<ast::Stmt>, diags: &mut Vec<Diagnostic>) {
        let mut plans: FxHashMap<(String, String), Plan> = FxHashMap::default();
        let mut skipped: FxHashSet<Span> = FxHashSet::default();
        for d in &self.defines {
            match self.check(d) {
                Ok(decl) => {
                    let (_, setter) = decl.accessors[&d.key];
                    let again = d.in_loop
                        || (self.defines.iter())
                            .filter(|o| o.object == d.object && o.key == d.key)
                            .count()
                            > 1;
                    let plan = plans
                        .entry((d.object.clone(), d.key.clone()))
                        .or_insert(Plan {
                            ty: decl.ty.clone(),
                            setter,
                            fixed: false,
                        });
                    plan.fixed |= d.fixes && again;
                }
                Err(Some(diag)) => {
                    diags.push(diag);
                    skipped.insert(d.span);
                }
                // Not a definition on a literal of this body: reported while checking.
                Err(None) => {}
            }
        }
        if !plans.is_empty() || !skipped.is_empty() {
            apply(stmts, &plans, &skipped);
        }
    }

    /// The declaration definition `d` changes (`Err(None)`: none this rewrite follows).
    fn check(&self, d: &Define) -> Result<&Decl, Option<Diagnostic>> {
        if self.repeated.contains(&d.object) {
            return Err(None);
        }
        let decl = self.decls.get(&d.object).ok_or(None)?;
        let (o, k) = (&d.object, &d.key);
        if d.data {
            return Err(Some(
                Diagnostic::error(
                    "a property descriptor with an accessor and `value` or `writable`",
                    d.span,
                )
                .with_note("Node throws `TypeError: Invalid property descriptor. Cannot both specify accessors and a value or writable attribute`"),
            ));
        }
        let (getter, setter) = decl.accessors.get(k).copied().unwrap_or_default();
        if !getter {
            return Err(Some(
                Diagnostic::error(
                    format!("`Object.defineProperty` of an accessor `{k}` on `{o}`, whose object literal has no getter `{k}`, is not supported yet"),
                    d.span,
                )
                .with_note(format!("Velt turns the accessors of the literal into ones the call can replace: write the property as `get {k}() {{ … }}` in the literal, and declare it in its interface")),
            ));
        }
        if d.set.is_some() && !setter {
            return Err(Some(
                Diagnostic::error(
                    format!("`Object.defineProperty` of a setter `{k}` on `{o}`, whose object literal has no setter `{k}`, is not supported yet"),
                    d.span,
                )
                .with_note(format!("give the literal a setter `set {k}(v) {{ … }}`, and declare it in its interface")),
            ));
        }
        Ok(decl)
    }
}

/// The object literal and the type a declaration gives it.
fn literal_of(v: &ast::VarDecl) -> Option<(&[ast::ObjectProp], ast::TypeExpr)> {
    let init = v.init.as_ref()?;
    match (&v.ty, &init.kind) {
        (Some(t), E::Object(props)) => Some((props, t.clone())),
        (None, E::Cast { expr, ty }) => match &expr.kind {
            E::Object(props) => Some((props, ty.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// A literal's `get k() { … }` (`true`) or `set k(v) { … }` (`false`), with its function.
fn accessor_fn(value: &ast::Expr) -> Option<(bool, &ast::Expr)> {
    let E::Call { callee, args, .. } = &value.kind else {
        return None;
    };
    let (E::Ident(f), [arrow]) = (&callee.kind, args.as_slice()) else {
        return None;
    };
    match f.name.as_str() {
        ast::OBJECT_GET_FN => Some((true, arrow)),
        ast::OBJECT_SET_FN => Some((false, arrow)),
        _ => None,
    }
}

/// Rewrite `stmts` (and the blocks inside them) per `plans`; the definitions at `skipped`
/// were reported and are dropped.
fn apply(
    stmts: &mut Vec<ast::Stmt>,
    plans: &FxHashMap<(String, String), Plan>,
    skipped: &FxHashSet<Span>,
) {
    let old = std::mem::take(stmts);
    for mut s in old {
        match &mut s.kind {
            S::Var(v) => {
                if let ast::PatternKind::Ident(name) = &v.pattern.kind {
                    let keys: Vec<(&String, &Plan)> = (plans.iter())
                        .filter(|((o, _), _)| *o == name.name)
                        .map(|((_, k), p)| (k, p))
                        .collect();
                    if !keys.is_empty() && v.kind == ast::VarKind::Const {
                        declaration(&name.name.clone(), v, &keys, s.span, stmts);
                        continue;
                    }
                }
            }
            S::Expr(e) => {
                if skipped.contains(&e.span) {
                    s.kind = S::Empty;
                } else if let Some(block) = definition(e, plans) {
                    s.kind = S::Block(block);
                }
            }
            _ => nested(&mut s, plans, skipped),
        }
        stmts.push(s);
    }
}

/// [`apply`] on the blocks of statement `s`.
fn nested(s: &mut ast::Stmt, plans: &FxHashMap<(String, String), Plan>, skipped: &FxHashSet<Span>) {
    let block = |b: &mut ast::Block| apply(&mut b.stmts, plans, skipped);
    match &mut s.kind {
        S::If { then, els, .. } => {
            block(then);
            if let Some(e) = els {
                let mut one = vec![std::mem::replace(
                    e.as_mut(),
                    ast::Stmt {
                        kind: S::Empty,
                        span: Span::DUMMY,
                    },
                )];
                apply(&mut one, plans, skipped);
                **e = wrap(one);
            }
        }
        S::While { body, .. } | S::DoWhile { body, .. } | S::ForOf { body, .. } => block(body),
        S::For { body, .. } => block(body),
        S::Block(b) => block(b),
        S::Labeled { body, .. } => nested(body, plans, skipped),
        S::Switch { cases, .. } => cases
            .iter_mut()
            .for_each(|c| apply(&mut c.body, plans, skipped)),
        S::Try {
            body,
            catch,
            finally,
        } => {
            block(body);
            if let Some((_, c)) = catch {
                block(c);
            }
            if let Some(f) = finally {
                block(f);
            }
        }
        _ => {}
    }
}

/// One statement, or a block of several.
fn wrap(mut stmts: Vec<ast::Stmt>) -> ast::Stmt {
    if stmts.len() == 1 {
        return stmts.remove(0);
    }
    let span = stmts.first().map_or(Span::DUMMY, |s| s.span);
    ast::Stmt {
        kind: S::Block(ast::Block { stmts, span }),
        span,
    }
}

/// The variables of each property, the declaration with the literal's accessors calling them,
/// and the accessors it had as their first values.
fn declaration(
    o: &str,
    v: &mut ast::VarDecl,
    keys: &[(&String, &Plan)],
    span: Span,
    out: &mut Vec<ast::Stmt>,
) {
    let mut after = vec![];
    for (k, plan) in keys {
        let member = member_type(&plan.ty, k);
        out.push(let_cell(&cell(o, k, "get"), getter_type(&member), span));
        if plan.setter {
            out.push(let_cell(&cell(o, k, "set"), setter_type(&member), span));
        }
        if plan.fixed {
            out.push(stmt(
                S::Var(ast::VarDecl {
                    kind: ast::VarKind::Let,
                    pattern: ident_pattern(&cell(o, k, "fixed"), span),
                    ty: None,
                    init: Some(expr(E::Lit(ast::Lit::Bool(false)), span)),
                    span,
                }),
                span,
            ));
        }
        let Some(E::Object(props)) = v.init.as_mut().map(literal_mut) else {
            continue;
        };
        for p in props.iter_mut() {
            let ast::ObjectProp::KeyValue(name, value) = p else {
                continue;
            };
            if name.name != **k {
                continue;
            }
            let Some((is_get, f)) = accessor_fn(value).map(|(g, f)| (g, f.clone())) else {
                continue;
            };
            let which = if is_get { "get" } else { "set" };
            let (forward, first) = if is_get {
                (get_forward(&cell(o, k, which), f.span), f)
            } else {
                let f = typed_setter(f, &member);
                (set_forward(&cell(o, k, which), &f, &member), f)
            };
            if let E::Call { args, .. } = &mut value.kind {
                args[0] = forward;
            }
            after.push(assign(&cell(o, k, which), first, span));
        }
    }
    out.push(stmt(S::Var(v.clone()), span));
    out.extend(after);
}

/// The literal of a declaration's initializer (inside an `as`).
fn literal_mut(e: &mut ast::Expr) -> &mut E {
    match &mut e.kind {
        E::Cast { expr, .. } => &mut expr.kind,
        k => k,
    }
}

/// `{ if (fixed) throw …; get = G; set = S; fixed = true; }` for a definition of a planned
/// property (`None`: another statement).
fn definition(e: &ast::Expr, plans: &FxHashMap<(String, String), Plan>) -> Option<ast::Block> {
    let (o, key, props) = accessor_call(e)?;
    let (E::Ident(o), E::Lit(ast::Lit::Str(k))) = (&o.kind, &key.kind) else {
        return None;
    };
    let plan = plans.get(&(o.name.clone(), k.clone()))?;
    let span = e.span;
    let member = member_type(&plan.ty, k);
    let mut stmts = vec![];
    if plan.fixed {
        let error = expr(
            E::New {
                class: named_type("TypeError", span),
                args: vec![expr(
                    E::Lit(ast::Lit::Str(format!("Cannot redefine property: {k}"))),
                    span,
                )],
            },
            span,
        );
        stmts.push(stmt(
            S::If {
                cond: ident(&cell(&o.name, k, "fixed"), span),
                then: ast::Block {
                    stmts: vec![stmt(S::Throw(error), span)],
                    span,
                },
                els: None,
            },
            span,
        ));
    }
    let mut fixes = false;
    for p in props {
        let ast::ObjectProp::KeyValue(n, v) = p else {
            continue;
        };
        match n.name.as_str() {
            "get" => stmts.push(assign(&cell(&o.name, k, "get"), v.clone(), span)),
            "set" => {
                let f = typed_setter(v.clone(), &member);
                stmts.push(assign(&cell(&o.name, k, "set"), f, span));
            }
            "configurable" => fixes = matches!(v.kind, E::Lit(ast::Lit::Bool(false))),
            _ => {}
        }
    }
    if plan.fixed && fixes {
        let t = expr(E::Lit(ast::Lit::Bool(true)), span);
        stmts.push(assign(&cell(&o.name, k, "fixed"), t, span));
    }
    Some(ast::Block { stmts, span })
}

/// The variable holding accessor `which` of property `k` of `o`.
fn cell(o: &str, k: &str, which: &str) -> String {
    format!("<{o}.{k}:{which}>")
}
