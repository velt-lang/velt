//! Constructor rules (TypeScript's): a derived constructor calls `super(...)` (TS2377), also
//! when no base class has a constructor. Statements that use neither `this` nor `super` may come
//! before it (`stmt` and `expr/supers` check its placement), also when the class has
//! initialized fields or parameter properties: those are set right after `super(...)` returns,
//! as in JavaScript (TypeScript 4.6+). It must run exactly once on every path: a statement of
//! the body itself, or one in each branch of an `if` / `else` (nested `if`s too), as TypeScript
//! accepts it, unless the class has parameter properties or no ancestor has a constructor and
//! some field has an initializer (those are set right after a root-level call). Every own field
//! without a default is assigned on every path.

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::stmt::is_super_call;
use super::FnCx;
use crate::defs::FnInfo;
use crate::hir::{self, DefId, ExprKind as H, LocalId, StmtKind as S, TyId};

impl FnCx<'_, '_> {
    /// Sets up the `super(...)` rules before a constructor body is checked.
    pub(super) fn ctor_begin(&mut self, f: &FnInfo, decl: &ast::FnDecl) {
        if f.owner.is_none() || self.this_base().is_none() {
            return;
        }
        // Until `super(...)`. Without a call on every path, the missing call is reported (not
        // every use of `this`).
        let stmts = &decl.body.stmts;
        self.f.before_super = stmts.iter().any(super_on_every_path);
        let mut nested = vec![];
        for st in stmts {
            if is_super_call(st) {
                self.f.super_sites.push(st.span);
            } else if super_on_every_path(st) {
                super_sites(st, &mut nested);
            }
        }
        if nested.is_empty() {
            return;
        }
        match self.branch_super_blocker(f, decl) {
            None => self.f.super_sites.extend(nested),
            Some(why) => {
                self.cx.error(
                    Diagnostic::error(
                        "`super(...)` must be a statement of the constructor's body itself in this class, not one in each branch of an `if`",
                        nested[0],
                    )
                    .with_note(why)
                    .with_note("call `super(...)` once before the `if`, choosing its arguments with a conditional: `super(c ? a : b);`"),
                );
                self.f.super_silent.extend(nested);
                // Reported once: not every use of `this` (a parameter property's store) too.
                self.f.before_super = false;
            }
        }
    }

    /// Why `super(...)` cannot be called in branches in constructor `f`'s class, if it cannot.
    /// After a call of a base class's constructor, wherever it is, this class's initializers
    /// run (lowering's `ctor_init`); parameter properties and, when no ancestor has a
    /// constructor, the initializers are placed after a root-level call.
    fn branch_super_blocker(&mut self, f: &FnInfo, decl: &ast::FnDecl) -> Option<String> {
        let a = self.cx.adt(f.owner?)?;
        let own = &a.fields[a.own_fields_start..];
        let class = a.name.clone();
        let mut props = decl.sig.params.iter();
        if let Some(p) = props.find(|p| own.iter().any(|fl| fl.span == p.name.span)) {
            return Some(format!(
                "`{}` is a parameter property of `{class}`, assigned right after the call",
                p.name.name
            ));
        }
        let initialized = a
            .fields
            .iter()
            .find(|fl| fl.has_default)
            .map(|fl| fl.name.clone());
        match (self.base_ctor(f), initialized) {
            (None, Some(name)) => Some(format!(
                "no base class of `{class}` has a constructor, so the initializer of the field `{name}` runs right after the call"
            )),
            _ => None,
        }
    }

    /// The constructor of the base class of constructor `f`'s class, if any.
    fn base_ctor(&mut self, f: &FnInfo) -> Option<DefId> {
        let a = self.cx.adt(f.owner?)?;
        let (b, _) = self.cx.class_of(a.base?)?;
        self.cx.adt(b).and_then(|x| x.ctor)
    }

    /// Constructor rules once the body is checked: `super(...)` called in a derived class,
    /// every own field without a default assigned on every path. Records what the field
    /// initializers the constructor runs itself may throw.
    pub(super) fn check_ctor(&mut self, f: &FnInfo, block: &hir::Block) {
        let Some(owner) = f.owner else { return };
        let base_ctor = self.base_ctor(f);
        let a = self.cx.adt(owner).expect("ICE: ctor owner");
        let needed: Vec<(u32, String)> = a.fields[a.own_fields_start..]
            .iter()
            .enumerate()
            .filter(|(_, fl)| !fl.has_default)
            .map(|(i, fl)| ((a.own_fields_start + i) as u32, fl.name.clone()))
            .collect();
        let class = a.name.clone();
        let base = a.base;
        if base_ctor.is_none() {
            // No ancestor has a constructor: this one runs every field initializer itself (with
            // one, they run after `super(...)`, which accounts for them).
            let this_ty = self.this_ty();
            for s in self.class_default_throws(this_ty, None, f.name_span) {
                self.throw_src(s);
            }
        }
        if let Some(base) = base.filter(|_| !self.f.super_called) {
            self.missing_super(f, &class, base);
        }
        let assigned = assigned_fields(block, LocalId(0));
        for (idx, name) in needed {
            if !assigned.contains(&idx) {
                self.cx.error(
                    Diagnostic::error(
                        format!(
                            "field `{name}` is not initialized by the constructor of `{class}`"
                        ),
                        f.name_span,
                    )
                    .with_note(format!("assign `this.{name} = ...` on every path")),
                );
            }
        }
    }

    /// TS2377: a derived constructor without `super(...)`, also when no base class has a
    /// constructor.
    fn missing_super(&mut self, f: &FnInfo, class: &str, base: TyId) {
        let bname = self.cx.display(base);
        let params: Vec<String> = self
            .cx
            .class_of(base)
            .and_then(|(b, _)| self.cx.adt(b).and_then(|x| x.ctor))
            .map(|c| {
                self.cx
                    .fn_info(c)
                    .params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let fix = match params.is_empty() {
            true => "add `super();` as the first statement of the constructor".to_string(),
            false => format!(
                "add `super({});` as the first statement of the constructor, with the arguments of `{bname}`'s constructor",
                params.join(", ")
            ),
        };
        self.cx.error(
            Diagnostic::error(
                format!("the constructor of `{class}` must call `super(...)`"),
                f.name_span,
            )
            .with_note(format!(
                "`{class}` extends `{bname}`, which must be initialized before `this` is used"
            ))
            .with_note(fix),
        );
    }
}

/// Does `s` call `super(...)` exactly once on every path: the call itself, an `if` / `else`
/// each of whose branches does, or a block with one such statement?
fn super_on_every_path(s: &ast::Stmt) -> bool {
    match &s.kind {
        _ if is_super_call(s) => true,
        ast::StmtKind::If {
            then,
            els: Some(els),
            ..
        } => once_in(&then.stmts) && super_on_every_path(els),
        ast::StmtKind::Block(b) => once_in(&b.stmts),
        _ => false,
    }
}

fn once_in(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().filter(|s| super_on_every_path(s)).count() == 1
}

/// The `super(...);` statements of `s` (which calls it on every path), appended to `out`.
fn super_sites(s: &ast::Stmt, out: &mut Vec<velt_common::Span>) {
    let branches: Vec<&[ast::Stmt]> = match &s.kind {
        _ if is_super_call(s) => return out.push(s.span),
        ast::StmtKind::If {
            then,
            els: Some(els),
            ..
        } => {
            super_sites(els, out);
            vec![&then.stmts]
        }
        ast::StmtKind::Block(b) => vec![&b.stmts],
        _ => return,
    };
    for st in branches.into_iter().flatten() {
        if super_on_every_path(st) {
            super_sites(st, out);
        }
    }
}

/// Fields of `this` assigned on every path through `b` (conservative).
fn assigned_fields(b: &hir::Block, this: LocalId) -> Vec<u32> {
    let mut out = vec![];
    for s in &b.stmts {
        match &s.kind {
            S::Expr(e) => field_assign(e, this, &mut out),
            S::Block(inner) => out.extend(assigned_fields(inner, this)),
            S::If {
                then,
                els: Some(els),
                ..
            } => {
                let (t, e) = (assigned_fields(then, this), assigned_fields(els, this));
                out.extend(t.into_iter().filter(|x| e.contains(x)));
            }
            _ => {}
        }
    }
    out
}

fn field_assign(e: &hir::Expr, this: LocalId, out: &mut Vec<u32>) {
    if let H::Assign { place, .. } = &e.kind {
        if let H::Field { base, index, .. } = &place.kind {
            if matches!(base.kind, H::Local(l, _) if l == this) {
                out.push(*index);
            }
        }
    }
}
