//! Intersection types of object types, `A & B` (design on #384): resolved here to one ordinary
//! anonymous object type, the type the user could write out, so nothing after sema sees them.
//!
//! - Fields are those of every operand, in TypeScript's order: `A`'s, then `B`'s new ones.
//! - A field in both gets the intersection of its types: identical types stay, object types
//!   merge, `T | null` & `U | null` is `(T & U) | null` (a field is required when any operand
//!   requires it), a literal & its base type is the literal; it is `readonly` only when every
//!   operand that has it says so.
//! - Unions distribute: `(A | B) & C` is `(A & C) | (B & C)`, without the members that have no
//!   value (`{ kind: "a" } & { kind: "b" }`), so `& { kind: "a" }` picks a member.
//! - A result with no value at all is an error, not a silent `never`.
//!
//! Operands are object types: anonymous ones, field-only interfaces, utility type results,
//! other intersections, and unions of these (with `null`). Classes, structs, interfaces with
//! methods, arrays, function types and type parameters are errors; a primitive with object
//! types is a branded type (`crate::brands`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, TyId, TyKind};
use crate::resolve::TyEnv;

/// How deep field types may nest before an intersection gives up (recursive object types).
const MAX_DEPTH: u32 = 32;

impl Ctx<'_> {
    /// `A & B & …` as written in `t`.
    pub(crate) fn resolve_intersection(
        &mut self,
        t: &ast::TypeExpr,
        parts: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        let mut operands: Vec<(TyId, Span)> = vec![];
        for p in parts {
            let ty = self.resolve_type(p, env);
            let ty = self.subst(ty, &env.args);
            if ty == self.ty.error {
                return ty;
            }
            operands.push((ty, p.span));
        }
        // A primitive with object types is a branded type.
        let bases: Vec<usize> = (0..operands.len())
            .filter(|&i| self.brandable(operands[i].0))
            .collect();
        let brand = match bases.as_slice() {
            [i] if operands.len() > 1 => Some(operands.remove(*i).0),
            _ => None,
        };
        let parts: Vec<&ast::TypeExpr> = match brand {
            Some(_) => parts
                .iter()
                .filter(|p| !operands_has_base(p, &bases, parts))
                .collect(),
            None => parts.iter().collect(),
        };
        for (&(ty, span), p) in operands.iter().zip(&parts) {
            if !self.intersectable(ty, span, written_name(p)) {
                return self.ty.error;
            }
        }
        let mut acc = operands[0].0;
        for &(b, _) in &operands[1..] {
            match self.meet(acc, b, env.module, 0) {
                Ok(m) => acc = m,
                Err(why) => {
                    let shown: Vec<String> =
                        operands.iter().map(|(o, _)| self.display(*o)).collect();
                    self.error(
                        Diagnostic::error(
                            format!("no value has type `{}`: {why}", shown.join(" & ")),
                            t.span,
                        )
                        .with_note("a value of an intersection has every part's fields at once"),
                    );
                    return self.ty.error;
                }
            }
        }
        self.declare_intersection_fields(acc, &operands);
        match brand {
            Some(base) => self.brand_type(base, acc, env.module, t.span),
            None => acc,
        }
    }

    /// Record where each field of the intersection `t` is written (for editors: go to
    /// definition, hover and its doc comment): in the first operand that has it.
    fn declare_intersection_fields(&mut self, t: TyId, operands: &[(TyId, Span)]) {
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return;
        };
        let Some(a) = self.adt(d) else {
            return;
        };
        let spans: Vec<Span> = a
            .fields
            .iter()
            .map(|f| {
                operands
                    .iter()
                    .find_map(|&(o, _)| self.written_field(o, &f.name))
                    .unwrap_or(Span::DUMMY)
            })
            .collect();
        self.declare_anon_fields(t, &spans);
    }

    /// Where the field `name` of the object type `t` is written, if it is.
    fn written_field(&self, t: TyId, name: &str) -> Option<Span> {
        let TyKind::Adt(d, _) = self.ty.kind(t) else {
            return None;
        };
        let f = self.adt(*d)?.fields.iter().find(|f| f.name == name)?;
        (f.span != Span::DUMMY).then_some(f.span)
    }

    /// Is `t` (written as the name `written`, if it is one) an operand `&` can combine
    /// (reported at `span` if not)?
    fn intersectable(&mut self, t: TyId, span: Span, written: Option<&str>) -> bool {
        if let Some(p) = self.ty.opt_payload(t) {
            return self.intersectable(p, span, None);
        }
        if let Some(members) = self.union_members(t) {
            return members
                .into_iter()
                .all(|m| self.intersectable(m, span, None));
        }
        if self.is_object_type(t) {
            return true;
        }
        let shown = written.map_or_else(|| self.display(t), str::to_string);
        let object = "write the fields as an object type `{ … }`";
        let (msg, note) = match self.ty.kind(t).clone() {
            TyKind::Adt(d, _) => match self.adt(d).map(|a| a.kind) {
                Some(AdtKind::Class) => (
                    format!("`&` combines object types; `{shown}` is a class"),
                    format!("Velt classes are nominal: use `Pick<{shown}, …>` for its fields, or a field-only interface"),
                ),
                Some(AdtKind::Struct) => (
                    format!("`&` combines object types; `{shown}` is a struct"),
                    format!("use `Pick<{shown}, …>` for its fields, or an object type"),
                ),
                _ => (
                    format!("`&` combines object types; found `{shown}`"),
                    object.into(),
                ),
            },
            TyKind::Dyn(..) => (
                format!("`&` combines object types; `{shown}` is an interface with methods"),
                "as a bound, `T extends A & B` combines interfaces".into(),
            ),
            TyKind::Array(_) | TyKind::Tuple(_) => (
                format!("`&` combines object types; `{shown}` is an array"),
                object.into(),
            ),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => (
                "intersections of function types (overloads) are not supported".to_string(),
                "declare one function type with the parameter types every call needs".into(),
            ),
            TyKind::Param(_) if self.checking_unused_aliases => return false,
            TyKind::Param(_) => (
                format!("`&` needs concrete object types; `{shown}` is a type parameter"),
                "intersections on type parameters are not supported yet (#350); write the concrete object types".into(),
            ),
            _ => (
                format!("`&` combines object types; `{shown}` is not one"),
                object.into(),
            ),
        };
        self.error(Diagnostic::error(msg, span).with_note(note));
        false
    }

    /// An anonymous object type, or a field-only interface's.
    pub(crate) fn is_object_type(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .adt(*d)
                .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(d)),
            _ => false,
        }
    }

    /// The intersection of `a` and `b`; `Err` says why no value has both types.
    fn meet(&mut self, a: TyId, b: TyId, module: usize, depth: u32) -> Result<TyId, String> {
        if a == b {
            return Ok(a);
        }
        if depth > MAX_DEPTH {
            return Err("the types nest too deeply to combine".into());
        }
        match (self.ty.opt_payload(a), self.ty.opt_payload(b)) {
            (Some(x), Some(y)) => {
                let m = self.meet(x, y, module, depth + 1)?;
                return Ok(self.ty.option(m));
            }
            (Some(x), None) => return self.meet(x, b, module, depth + 1),
            (None, Some(y)) => return self.meet(a, y, module, depth + 1),
            (None, None) => {}
        }
        for (u, other, swap) in [(a, b, false), (b, a, true)] {
            if let Some(members) = self.union_members(u) {
                return self.meet_union(&members, other, swap, module, depth);
            }
        }
        if self.is_object_type(a) && self.is_object_type(b) {
            return self.merge_objects(a, b, module, depth);
        }
        for (l, base) in [(a, b), (b, a)] {
            if let Some(v) = self.lit_value(l) {
                if self.lit_base(&v) == base {
                    return Ok(l);
                }
            }
        }
        let (sa, sb) = (self.display(a), self.display(b));
        Err(format!("`{sa}` and `{sb}` have no value in common"))
    }

    /// `(M1 | M2 | …) & other` (or `other & (…)` when `swap`): the members that meet `other`.
    fn meet_union(
        &mut self,
        members: &[TyId],
        other: TyId,
        swap: bool,
        module: usize,
        depth: u32,
    ) -> Result<TyId, String> {
        let mut out = vec![];
        let mut first_err = None;
        for &m in members {
            let r = match swap {
                false => self.meet(m, other, module, depth + 1),
                true => self.meet(other, m, module, depth + 1),
            };
            match r {
                Ok(t) => out.push(t),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        match out.len() {
            0 => Err(first_err.unwrap_or_default()),
            1 => Ok(out[0]),
            _ => Ok(self.union_of(&out, false, Span::DUMMY)),
        }
    }

    /// The object type with the fields of both `a` and `b` (see the module docs).
    fn merge_objects(
        &mut self,
        a: TyId,
        b: TyId,
        module: usize,
        depth: u32,
    ) -> Result<TyId, String> {
        let (fa, fb) = (self.public_fields(a), self.public_fields(b));
        let mut out = fa;
        for f in fb {
            let Some(i) = out.iter().position(|g| g.name == f.name) else {
                out.push(f);
                continue;
            };
            let g = out[i].clone();
            let ty = match self.meet(g.ty, f.ty, module, depth + 1) {
                Ok(ty) => ty,
                Err(_) => {
                    let (ga, fb) = (self.display(g.ty), self.display(f.ty));
                    return Err(format!(
                        "field `{}` is `{ga}` in one part and `{fb}` in another",
                        f.name
                    ));
                }
            };
            out[i] = ShapeField {
                name: g.name,
                ty,
                readonly: g.readonly && f.readonly,
                optional: g.optional && f.optional,
            };
        }
        Ok(self.anon_type_with(&out, module))
    }

    /// The fields of object type `t`, its type arguments substituted.
    fn public_fields(&mut self, t: TyId) -> Vec<ShapeField> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return vec![];
        };
        let Ok(fields) = self.fields_now(d) else {
            return vec![];
        };
        fields
            .into_iter()
            .filter(|(_, public)| *public)
            .map(|(f, _)| ShapeField {
                ty: self.subst(f.ty, &args),
                ..f
            })
            .collect()
    }
}

/// The name an operand is written as (`T` in `T & { … }`), for messages.
fn written_name(t: &ast::TypeExpr) -> Option<&str> {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } if path.len() == 1 => Some(&path[0].name),
        _ => None,
    }
}

/// Is `p` the operand at the one index of `bases` (the primitive of a branded type)?
fn operands_has_base(p: &ast::TypeExpr, bases: &[usize], parts: &[ast::TypeExpr]) -> bool {
    bases.first().is_some_and(|&i| std::ptr::eq(p, &parts[i]))
}
