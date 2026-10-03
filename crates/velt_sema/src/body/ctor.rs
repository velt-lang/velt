//! Constructor rules (TypeScript's): a derived constructor calls `super(...)` exactly once, as a
//! statement of its own in the constructor body. Statements that use neither `this` nor `super`
//! may come before it, unless the class has initialized fields or parameter properties (which are
//! set right after `super(...)` returns): then `super(...)` comes first. Every own field without
//! a default is assigned on every path.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::defs::{FnInfo, FnKind, ThrowSrc};
use crate::hir::{self, DefId, ExprKind as H, LocalId, StmtKind as S, TyId};

/// `super(...)` bookkeeping of a constructor frame.
#[derive(Default)]
pub(crate) struct SuperState {
    /// The class extends another: its constructor must call `super(...)`.
    pub derived: bool,
    /// The statement being checked is a top-level `super(...);` (`ok`: its call is allowed).
    pub in_super_stmt: bool,
    pub ok: bool,
    /// Statement nesting depth (0: between the constructor body's statements).
    pub depth: u32,
    /// Top-level statements checked so far.
    pub top_stmts: u32,
    pub called: bool,
    /// The call was nested in another statement or expression (reported as such).
    pub called_nested: bool,
    /// Why `super(...)` must be the first statement: an initialized field or a parameter
    /// property (its description and span).
    pub first: Option<(String, Span)>,
}

impl FnCx<'_, '_> {
    /// Sets up the `super(...)` rules before a constructor body is checked.
    pub(super) fn ctor_begin(&mut self, f: &FnInfo, decl: &ast::FnDecl) {
        let Some(owner) = f.owner else { return };
        let Some(a) = self.cx.adt(owner) else { return };
        self.f.sup.derived = a.base.is_some();
        if !self.f.sup.derived {
            return;
        }
        self.f.sup.first = a.decl.and_then(|t| first_reason(t, decl));
        // Without a top-level call, the missing call is reported (not every use of `this`).
        self.before_super = decl.body.stmts.iter().any(is_super_call);
    }

    /// Enters a statement: returns whether it is a top-level statement of a constructor.
    pub(super) fn ctor_stmt_enter(&mut self, s: &ast::Stmt) -> bool {
        let top = self.f.kind == FnKind::Ctor && self.f.sup.depth == 0;
        self.f.sup.depth += 1;
        if top {
            self.f.sup.in_super_stmt = self.f.sup.derived && is_super_call(s);
            self.f.sup.ok = self.f.sup.in_super_stmt;
        }
        top
    }

    pub(super) fn ctor_stmt_leave(&mut self, top: bool) {
        self.f.sup.depth -= 1;
        self.f.sup.ok = false;
        if top {
            self.f.sup.top_stmts += 1;
            self.f.sup.in_super_stmt = false;
        }
    }

    /// Takes the permission to call `super(...)` here, reporting a call that comes after other
    /// statements in a class that needs it first.
    pub(super) fn take_super_ok(&mut self, span: Span) -> bool {
        let ok = std::mem::take(&mut self.f.sup.ok) && !self.f.sup.called;
        if !ok {
            return false;
        }
        self.f.sup.called = true;
        if let (true, Some((why, at))) = (self.f.sup.top_stmts > 0, self.f.sup.first.clone()) {
            let class = self.owner_class_name();
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "`super(...)` must be the first statement of the constructor of `{class}`"
                    ),
                    span,
                )
                .with_label(at, format!("{why} is set right after `super(...)` returns"))
                .with_note(format!(
                    "`{class}` has {why}, so no statement may run before `super(...)`"
                ))
                .with_note("move the statements before `super(...)` after it"),
            );
        }
        true
    }

    /// Reports a `super(...)` call where it is not allowed (`derived`: the class extends another).
    pub(super) fn misplaced_super(&mut self, derived: bool, span: Span) {
        let ctor_frame = self.outer.iter().rposition(|f| f.kind == FnKind::Ctor);
        let d = if self.f.kind == FnKind::Closure && ctor_frame.is_some() {
            // The constructor itself is reported here, not as missing its call.
            if let Some(i) = ctor_frame {
                self.outer[i].sup.called = true;
            }
            Diagnostic::error("`super(...)` cannot be called inside a function", span).with_note(
                "the base constructor must run exactly once, before `this` is used: call `super(...)` in the constructor itself",
            )
        } else if !derived {
            Diagnostic::error(
                "`super(...)` can only be called in the constructor of a class that `extends` another",
                span,
            )
        } else if self.f.kind != FnKind::Ctor {
            Diagnostic::error("`super(...)` can only be called in a constructor", span)
                .with_note("to call a base class method, write `super.method(...)`")
        } else if self.f.sup.called && (self.f.sup.in_super_stmt || !self.f.sup.called_nested) {
            Diagnostic::error("`super(...)` is called more than once", span)
                .with_note("the base constructor must run exactly once; remove this call")
        } else {
            // The constructor does call `super(...)`, just not exactly once on every path.
            self.f.sup.called = true;
            self.f.sup.called_nested = true;
            Diagnostic::error(
                "`super(...)` must be a statement of its own in the constructor body",
                span,
            )
            .with_note("inside a condition, a loop or another expression, the base constructor could run never or more than once")
            .with_note("call `super(...);` unconditionally, and compute its arguments with expressions such as `c ? a : b`")
        };
        self.cx.error(d);
    }

    /// `return` in a derived constructor before `super(...)` has run.
    pub(super) fn check_return_in_ctor(&mut self, span: Span) {
        if self.f.kind == FnKind::Ctor && self.before_super {
            let class = self.owner_class_name();
            self.cx.error(
                Diagnostic::error(
                    format!("the constructor of `{class}` returns before calling `super(...)`"),
                    span,
                )
                .with_note("the base class must be initialized on every path: call `super(...)` before this `return`, or `throw` instead"),
            );
        }
    }

    /// `this` (or `super.m()`) before `super(...)` has run: the object is not initialized yet.
    pub(crate) fn check_this_ready(&mut self, span: Span) {
        if self.before_super {
            self.cx.error(
                Diagnostic::error("`this` cannot be used before `super(...)` has run", span)
                    .with_note("the base constructor has not initialized the object yet: use `this` after the `super(...)` call"),
            );
        }
    }

    fn owner_class_name(&self) -> String {
        self.owner
            .and_then(|o| self.cx.adt(o))
            .map(|a| a.name.clone())
            .unwrap_or_default()
    }

    /// Constructor rules once the body is checked: `super(...)` called in a derived class,
    /// every own field without a default assigned on every path. Records what the field
    /// defaults the constructor evaluates may throw.
    pub(super) fn check_ctor(&mut self, f: &FnInfo, block: &hir::Block) {
        let Some(owner) = f.owner else { return };
        let a = self.cx.adt(owner).expect("ICE: ctor owner");
        let needed: Vec<(u32, String)> = a.fields[a.own_fields_start..]
            .iter()
            .enumerate()
            .filter(|(_, fl)| !fl.has_default)
            .map(|(i, fl)| ((a.own_fields_start + i) as u32, fl.name.clone()))
            .collect();
        let class = a.name.clone();
        if let Some(base) = a.base.filter(|_| !self.f.sup.called) {
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
        if let Some(this) = f.this.as_ref().map(|t| t.ty) {
            for s in self.ctor_default_throws(this, f.name_span) {
                self.throw_src(s);
            }
        }
    }

    /// TS2377: a derived constructor without `super(...)`.
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

    /// What the field defaults a constructor of class type `this` evaluates may throw: its own,
    /// and those of the base classes that `super(...)` initializes without a constructor of
    /// their own (lowering runs them right after the base constructor returns).
    fn ctor_default_throws(&mut self, this: TyId, span: Span) -> Vec<ThrowSrc> {
        let stop: Option<DefId> = self
            .cx
            .base_of(this)
            .and_then(|b| self.cx.class_of(b))
            .and_then(|(b, _)| self.cx.adt(b).and_then(|a| a.ctor))
            .and_then(|c| self.cx.fn_info(c).owner);
        let mut out = vec![];
        let mut cur = Some(this);
        while let Some(ty) = cur {
            let Some((d, args)) = self.cx.class_of(ty) else {
                break;
            };
            if Some(d) == stop {
                break;
            }
            crate::body::field_defaults(self.cx, d);
            let Some(a) = self.cx.adt(d) else { break };
            let own: Vec<ThrowSrc> = a.fields[a.own_fields_start..]
                .iter()
                .flat_map(|f| f.default_throws.iter().cloned())
                .collect();
            out.extend(
                own.iter()
                    .map(|s| s.used_at(span, |t| self.cx.ty.subst(t, &args))),
            );
            cur = self.cx.base_of(ty);
        }
        out
    }
}

/// Why `super(...)` must come first in class `t` with constructor `ctor`: its first
/// initialized field or parameter property.
fn first_reason(t: &ast::TypeDecl, ctor: &ast::FnDecl) -> Option<(String, Span)> {
    t.fields.iter().filter(|f| !f.is_static).find_map(|f| {
        if ctor.sig.params.iter().any(|p| p.span == f.span) {
            Some((format!("the parameter property `{}`", f.name.name), f.span))
        } else {
            f.default
                .as_ref()
                .map(|_| (format!("the initialized field `{}`", f.name.name), f.span))
        }
    })
}

/// `super(...);`
pub(super) fn is_super_call(s: &ast::Stmt) -> bool {
    let ast::StmtKind::Expr(e) = &s.kind else {
        return false;
    };
    matches!(&e.kind, ast::ExprKind::Call { callee, .. } if matches!(callee.kind, ast::ExprKind::Super))
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
