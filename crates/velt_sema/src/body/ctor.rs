//! Constructor rules (TypeScript's): a derived constructor calls `super(...)` (TS2377), also
//! when no base class has a constructor. Statements that use neither `this` nor `super` may come
//! before it (`stmt` and `expr/supers` check its placement), unless the class has initialized
//! fields or parameter properties, which are set right after `super(...)` returns: then
//! `super(...)` comes first (TS2376). Every own field without a default is assigned on every
//! path.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::stmt::is_super_call;
use super::FnCx;
use crate::defs::FnInfo;
use crate::hir::{self, DefId, ExprKind as H, LocalId, StmtKind as S, TyId};

impl FnCx<'_, '_> {
    /// Sets up the `super(...)` rules before a constructor body is checked.
    pub(super) fn ctor_begin(&mut self, f: &FnInfo, decl: &ast::FnDecl) {
        let Some(owner) = f.owner else { return };
        if self.this_base().is_none() {
            return;
        }
        self.f.super_first = self
            .cx
            .adt(owner)
            .and_then(|a| a.decl)
            .and_then(|t| first_reason(t, decl));
        // Until `super(...)`. Without a root-level call, the missing call is reported (not every
        // use of `this`).
        self.f.before_super = decl.body.stmts.iter().any(is_super_call);
    }

    /// A `super(...)` call after other statements in a class that needs it first (TS2376).
    pub(crate) fn check_super_first(&mut self, span: Span) {
        let Some((why, at)) = self.f.super_first.clone() else {
            return;
        };
        if self.f.root_stmts == 0 {
            return;
        }
        let class = self.owner_class_name();
        self.cx.error(
            Diagnostic::error(
                format!("`super(...)` must be the first statement of the constructor of `{class}`"),
                span,
            )
            .with_label(at, format!("{why} is set right after `super(...)` returns"))
            .with_note(format!(
                "`{class}` has {why}, so no statement may run before `super(...)`"
            ))
            .with_note("move the statements before `super(...)` after it"),
        );
    }

    fn owner_class_name(&self) -> String {
        self.owner
            .and_then(|o| self.cx.adt(o))
            .map(|a| a.name.clone())
            .unwrap_or_default()
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
