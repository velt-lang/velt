//! Member access (fields, `length`, unit enum variants), indexing and `as` casts.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::Bound;
use crate::hir::{self, AdtKind, Callee, DefId, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};
use crate::ide::record::Target;

fn base_mode(want: Want) -> UseMode {
    if want == Want::BorrowMut {
        UseMode::BorrowMut
    } else {
        UseMode::Borrow
    }
}

impl FnCx<'_, '_> {
    /// Struct / class / anonymous-object type: (def, type args).
    pub(crate) fn adt_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, args) if self.cx.adt(*d).is_some() => Some((*d, args.clone())),
            _ => None,
        }
    }

    /// Field `name` of struct / class / object values of type `t`: (index, field type).
    pub(crate) fn field_of(&mut self, t: TyId, name: &str) -> Option<(u32, TyId)> {
        self.cx.field_of(t, name)
    }

    /// `x.field` on an interface value or bounded generic: a call of the field's getter slot
    /// (see `collect::getters`); the result is an owned copy / clone.
    fn iface_field(
        &mut self,
        mut obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (bounds, on_param) = match self.cx.ty.kind(obj.ty) {
            TyKind::Dyn(d, args) => (
                vec![Bound {
                    iface: *d,
                    args: args.clone(),
                }],
                false,
            ),
            TyKind::Param(n) => (
                self.bounds.get(*n as usize).cloned().unwrap_or_default(),
                true,
            ),
            _ => return Err(obj),
        };
        for b in bounds {
            let Some(i) = self.cx.iface(b.iface) else {
                continue;
            };
            let Some(idx) = i.fields.iter().position(|f| f.name == prop.name) else {
                continue;
            };
            let (fty, slot) = (i.fields[idx].ty, (i.methods.len() + idx) as u32);
            self.cx
                .rec_ref(prop.span, Target::Field(b.iface, idx as u32));
            let fty = self.cx.ty.subst(fty, &b.args);
            if want == Want::BorrowMut {
                self.cx.err(
                    format!(
                        "cannot modify `{}` through an interface or generic value",
                        prop.name
                    ),
                    prop.span,
                );
                return Ok(self.error_expr(span));
            }
            set_place_mode(&mut obj, UseMode::Borrow);
            let callee = if on_param {
                Callee::ParamMethod {
                    iface: b.iface,
                    iface_args: b.args.clone(),
                    slot,
                    method_type_args: vec![],
                }
            } else {
                Callee::Dyn { slot }
            };
            let kind = H::Call {
                callee,
                args: vec![obj],
            };
            return Ok(self.mk(kind, fty, span));
        }
        Err(obj)
    }

    /// `obj.prop` as a field place used per `want`.
    pub(crate) fn field_access(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if obj.ty == self.cx.ty.never {
            // A value narrowed to `never` (a read that panics): its fields are unreachable too.
            return Some(obj);
        }
        if self.cx.ty.is_bottom(obj.ty) {
            return None;
        }
        if self.cx.ty.opt_payload(obj.ty).is_some() {
            let tn = self.cx.display(obj.ty);
            self.cx.error(
                Diagnostic::error(format!("value of type `{tn}` may be null"), obj.span)
                    .with_note(format!("use `?.{}` or check `!= null` first", prop.name)),
            );
            return None;
        }
        let mut obj = match self.iface_field(obj, prop, want, span) {
            Ok(call) => return Some(call),
            Err(obj) => obj,
        };
        if self.cx.union_def(obj.ty).is_some() {
            match self.union_field(obj, prop, want, span) {
                Ok(h) => return Some(h),
                Err(o) => obj = o,
            }
        }
        let Some((index, fty)) = self.field_of(obj.ty, &prop.name) else {
            self.no_field(obj.ty, prop);
            return None;
        };
        self.check_field_private(obj.ty, index, prop);
        if let Some((d, _)) = self.adt_of(obj.ty) {
            self.cx.rec_ref(prop.span, Target::Field(d, index));
        }
        set_place_mode(&mut obj, base_mode(want));
        let mode = self.use_mode(fty, want);
        Some(self.mk(
            H::Field {
                base: Box::new(obj),
                index,
                mode,
            },
            fty,
            span,
        ))
    }

    fn no_field(&mut self, t: TyId, prop: &ast::Ident) {
        if self.has_setter(t, &prop.name) {
            return self.cx.err(
                format!("cannot read `{}`: it has a setter but no getter", prop.name),
                prop.span,
            );
        }
        let tn = self.cx.display(t);
        let mut d = Diagnostic::error(
            format!("no field `{}` on type `{tn}`", prop.name),
            prop.span,
        );
        if self.adt_of(t).is_some() {
            d = d.with_note(
                "objects have a fixed shape: fields cannot be added at runtime; use a `Map<string, V>` for dynamic keys",
            );
        }
        if let Some(note) = self.narrowing_note(t) {
            d = d.with_note(note);
        }
        if self.method_exists(t, &prop.name) {
            d = d.with_note(format!(
                "`{}` is a method: call it with `.{}()`",
                prop.name, prop.name
            ));
        }
        self.cx.error(d);
    }

    pub(crate) fn member(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        optional: bool,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        match self.namespace_member(object, prop) {
            Some(id) if !optional => return self.ident_expr(&id, exp, want),
            _ => {}
        }
        if let Some(h) = self
            .process_env_member(object, prop, exp, span)
            .filter(|_| !optional)
        {
            return h;
        }
        if let Some(object) = self.without_namespace(object) {
            return self.member(&object, prop, optional, exp, want, span);
        }
        if let ast::ExprKind::Ident(id) = &object.kind {
            if !self.is_local_name(&id.name) {
                if matches!(
                    id.name.as_str(),
                    "console" | "process" | "Promise" | "performance" | "Date"
                ) && self.lookup_item(&id.name, id.span).is_none()
                {
                    let what = format!("`{}.{}`", id.name, prop.name);
                    let msg = match (id.name.as_str(), prop.name.as_str()) {
                        ("process", "stdout" | "stderr" | "env") => {
                            format!("{what} is not a value: use its members")
                        }
                        ("process", _) => format!("{what} is not supported"),
                        _ => format!("{what} can only be called"),
                    };
                    let mut d = Diagnostic::error(msg, span);
                    if id.name == "process" {
                        d = d.with_note(process_note(&prop.name));
                    }
                    self.cx.error(d);
                    return self.error_expr(span);
                }
                let item = self.lookup_item(&id.name, id.span);
                if let Some(c) = item.and_then(|it| self.companion_class(&id.name, it)) {
                    if let Some(h) = self.static_field(c, prop, want, span) {
                        return h;
                    }
                }
                if let Some(Item::Def(d)) = item {
                    if self.cx.enum_info(d).is_some() {
                        return self.variant_value(d, prop, &[], exp, span);
                    }
                    if let Some(h) = self.static_field(d, prop, want, span) {
                        return h;
                    }
                    if self.cx.adt(d).is_some() {
                        self.cx.err(
                            format!(
                                "`{}.{}` is not a value; static members must be called",
                                id.name, prop.name
                            ),
                            span,
                        );
                        return self.error_expr(span);
                    }
                }
            }
        }
        if optional {
            return self.optional_chain(object, span, |s, v| s.member_of(v, prop, want, span));
        }
        let obj = self.expr(object, None, Want::Borrow);
        let h = self.member_of(obj, prop, want, span);
        self.narrowed_field(object, prop, h, want)
    }

    /// `.prop` on an already checked value.
    pub(crate) fn member_of(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        if self.record_args(obj.ty).is_some() {
            return self.record_read(obj, super::record::RecordKey::Name(prop), span);
        }
        let obj = self.widen_literal_receiver(obj, &prop.name);
        if prop.name == "length" {
            let t = obj.ty;
            if t == self.cx.ty.str_ {
                return self.intrinsic(Intrinsic::StrLen, vec![obj], self.cx.ty.usize, span);
            }
            if self.cx.ty.array_elem(t).is_some() {
                return self.intrinsic(Intrinsic::ArrayLen, vec![obj], self.cx.ty.usize, span);
            }
        }
        let obj = match self.getter_read(obj, prop, span) {
            Ok(call) => return call,
            Err(obj) => obj,
        };
        match self.field_access(obj, prop, want, span) {
            Some(h) => h,
            None => self.error_expr(span),
        }
    }

    pub(crate) fn index_expr(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        optional: bool,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        if optional {
            return self.optional_chain(object, span, |s, v| s.index_of(v, index, want, span));
        }
        if let Some(h) = self.process_env_index(object, index, span) {
            return h;
        }
        let obj = self.expr(object, None, Want::Borrow);
        self.index_of(obj, index, want, span)
    }

    pub(super) fn index_of(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let t = obj.ty;
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(..) if self.record_args(t).is_some() => {
                self.record_read(obj, super::record::RecordKey::Index(index), span)
            }
            TyKind::Array(elem) => self.array_index(obj, elem, index, want, span),
            TyKind::Tuple(ts) => self.tuple_index(obj, &ts, index, want, span),
            TyKind::Error | TyKind::Never => {
                self.expr(index, None, Want::Borrow);
                self.error_expr(span)
            }
            _ => {
                let tn = self.cx.display(t);
                let mut d =
                    Diagnostic::error(format!("cannot index a value of type `{tn}`"), obj.span);
                if self.cx.class_of(t).is_some() {
                    d = d.with_note("use a method such as `m.get(key)`");
                }
                self.cx.error(d);
                self.expr(index, None, Want::Borrow);
                self.error_expr(span)
            }
        }
    }

    /// `xs[i]`: the index is converted to `usize`.
    fn array_index(
        &mut self,
        mut obj: hir::Expr,
        elem: TyId,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let usize_ = self.cx.ty.usize;
        let mut i = self.expr(index, Some(usize_), Want::Borrow);
        if self.cx.ty.is_int(i.ty) && i.ty != usize_ {
            let is = i.span;
            i = self.mk(H::Cast(Box::new(i)), usize_, is);
        } else if !self.cx.ty.is_bottom(i.ty) && i.ty != usize_ {
            let tn = self.cx.display(i.ty);
            let mut d = velt_common::Diagnostic::error(
                format!("array index must be an integer, found `{tn}`"),
                i.span,
            );
            if let Some(note) = self.float_division_note(&i) {
                d = d.with_note(note);
            }
            self.cx.error(d);
        }
        set_place_mode(&mut obj, base_mode(want));
        let mode = self.use_mode(elem, want);
        let kind = H::Index {
            base: Box::new(obj),
            index: Box::new(i),
            mode,
        };
        self.mk(kind, elem, span)
    }

    /// `pair[0]` with a literal index: a field of the tuple.
    fn tuple_index(
        &mut self,
        mut obj: hir::Expr,
        ts: &[TyId],
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let n = match &index.kind {
            ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => Some(*value as usize),
            _ => None,
        };
        let Some((n, ety)) = n.and_then(|n| ts.get(n).map(|t| (n, *t))) else {
            self.cx.err(
                "tuple index must be an integer literal within the tuple's length",
                index.span,
            );
            return self.error_expr(span);
        };
        set_place_mode(&mut obj, base_mode(want));
        let mode = self.use_mode(ety, want);
        let kind = H::Field {
            base: Box::new(obj),
            index: n as u32,
            mode,
        };
        self.mk(kind, ety, span)
    }

    pub(crate) fn cast(&mut self, expr: &ast::Expr, ty: &ast::TypeExpr, span: Span) -> hir::Expr {
        let target = self.resolve(ty);
        let inner = self.expr(expr, None, Want::Borrow);
        // A literal type casts as its base type (`k as f64` with `k: 1 | 2`).
        let inner = self.widen_value(inner);
        let src = inner.ty;
        let t = &self.cx.ty;
        let c_like = self.is_c_like_enum(src) && !self.is_string_enum(src);
        let ok = t.is_bottom(src)
            || t.is_bottom(target)
            || (t.is_numeric(src) && t.is_numeric(target))
            || (src == t.bool_ && t.is_int(target))
            || (c_like && t.is_int(target))
            || src == target && (t.is_numeric(src) || src == t.bool_);
        if !ok {
            let (s, d) = (self.cx.display(src), self.cx.display(target));
            self.cx.err(format!("cannot cast `{s}` as `{d}`"), span);
            return self.error_expr(span);
        }
        self.mk(H::Cast(Box::new(inner)), target, span)
    }

    /// Enum without payloads (casts to its discriminant).
    fn is_c_like_enum(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .enum_info(*d)
                .is_some_and(|e| e.variants.iter().all(|v| v.payload.is_empty())),
            _ => false,
        }
    }

    /// Is `t` a class type (for "use `new`" hints)?
    pub(crate) fn is_class_def(&self, d: DefId) -> bool {
        self.cx.adt(d).is_some_and(|a| a.kind == AdtKind::Class)
    }
}

/// Where Node's `process.<name>` lives in Velt: the builtin `process` namespace has `exit`,
/// `memoryUsage()`, `stdout.write`, `stderr.write` and `env` (`super::process`); the rest is in
/// `velt:process`.
fn process_note(name: &str) -> String {
    match name {
        "stdout" | "stderr" => format!(
            "call `process.{name}.write(s)`; for bytes, `import {{ stdout }} from \"velt:process\"`"
        ),
        "env" => "read a variable with `process.env.NAME` or `process.env[name]` \
                  (`string | null`); set one with `setEnv(name, value)` of `velt:process`"
            .to_string(),
        "argv" => "use `args()` from `velt:process`: the arguments after the program, like \
                   Node's `process.argv.slice(2)`"
            .to_string(),
        "cwd" | "chdir" => {
            format!("use `import {{ {name} }} from \"velt:process\"`: `{name}` is a function there")
        }
        _ => "use the functions of `velt:process` (`args()`, `env(name)`, `cwd()`, \
              `stdout.write(s)`)"
            .to_string(),
    }
}
