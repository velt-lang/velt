//! Member access (fields, `length`, unit enum variants), indexing and `as` casts.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

pub(crate) use super::member_keys::{literal_key, member_view};

use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};
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
        // A class and its subclass may each have a field `#x` (`Ctx::field_seen_from`).
        let owner = self.name_owner(name);
        self.cx.field_seen_from(t, name, owner)
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
            // `null` for a `T | null`, `undefined` for a `T | undefined`.
            let what = self.cx.ty.nothing_of(obj.ty).map_or("null", |n| n.word());
            self.cx.error(
                Diagnostic::error(format!("value of type `{tn}` may be {what}"), obj.span)
                    .with_note(format!("use `?.{}` or check `!= null` first", prop.name)),
            );
            return None;
        }
        // `x.f = v` on an `any` (reads were handled by `member_value`).
        let obj = match self.dynamic_get(obj, &prop.name, want, span) {
            Ok(h) => return Some(h),
            Err(obj) => obj,
        };
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
        let renamed = self.ctor_member(obj.ty, prop); // `C.name` of a class value
        let prop = renamed.as_ref().unwrap_or(prop);
        let Some((index, fty)) = self.field_of(obj.ty, &prop.name) else {
            self.no_field(obj.ty, obj.span, prop);
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

    /// Is `length` of a `t` the intrinsic length of a string or array?
    pub(super) fn has_length(&self, t: TyId) -> bool {
        t == self.cx.ty.str_ || self.cx.ty.array_elem(t).is_some()
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
        if !optional {
            if let Some(h) = self.process_env_member(object, prop, exp, span) {
                return h;
            }
            if let Some(h) = self.process_argv_member(object, prop, exp, span) {
                return h;
            }
        }
        if let Some(object) = self.without_namespace(object) {
            return self.member(&object, prop, optional, exp, want, span);
        }
        if let (ast::ExprKind::This, Some((this, declaring))) = (&object.kind, self.static_this) {
            // `this.NAME` / `this.f` in a static method: the class it was called on.
            if let Some(h) = self.this_static_field(this, declaring, prop, want, span) {
                return h;
            }
            if let Some(h) = self.static_method_value(this, this, prop, exp, span) {
                return h;
            }
            let cname = self
                .cx
                .adt(this)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            self.cx.err(
                format!("`{cname}` has no static member `{}`", prop.name),
                prop.span,
            );
            return self.error_expr(span);
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
                        ("process", p) if !matches!(p, "exit" | "memoryUsage") => {
                            format!("{what} is not supported")
                        }
                        _ => format!("{what} can only be called"),
                    };
                    let mut d = Diagnostic::error(msg, span);
                    if id.name == "process" && !matches!(prop.name.as_str(), "exit" | "memoryUsage")
                    {
                        d = d.with_note(super::process::process_note(&prop.name));
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
                    if let Some(h) = self.static_method_value(d, d, prop, exp, span) {
                        return h;
                    }
                    if let Some(h) = self.class_name_read(d, prop, span) {
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
        // A union local narrowed to one member is read as the union here, so that a view it may
        // hold is read through its getters (`union_view`).
        self.view_receiver = want != Want::BorrowMut && is_name(object);
        let obj = self.expr(object, None, Want::Borrow);
        self.view_receiver = false;
        if want != Want::BorrowMut && self.is_method_value(obj.ty, &prop.name) {
            return self.method_value(obj, object, prop, exp, span);
        }
        let r = self.in_place_receiver(object, obj);
        let h = self.member_of(r.recv, prop, want, span);
        let h = self.after_receiver(r.before, h);
        let h = self.narrowed_field(object, prop, h, want);
        self.downcast_field(object, prop, h)
    }

    /// `.prop` on an already checked value.
    pub(crate) fn member_of(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        // `xs.length`, a `size` getter: a number in user code (`numbers`).
        let h = self.member_value(obj, prop, want, span);
        self.std_number(h)
    }

    fn member_value(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let obj = match self.dynamic_get(obj, &prop.name, want, span) {
            Ok(h) => return h,
            Err(obj) => obj,
        };
        if self.record_args(obj.ty).is_some() {
            return self.record_member_read(obj, prop, span);
        }
        let obj = self.unbrand(obj);
        let obj = self.widen_literal_receiver(obj, &prop.name);
        // `s.length` on a `shared<string>` or `shared<T[]>`: the length of the value inside.
        let obj = match self.cx.ty.kind(obj.ty) {
            TyKind::Shared(t) if prop.name == "length" && self.has_length(*t) => {
                self.frozen_view(obj, Some("length"))
            }
            _ => obj,
        };
        if prop.name == "length" {
            let t = obj.ty;
            if t == self.cx.ty.str_ {
                return self.intrinsic(Intrinsic::StrLen, vec![obj], self.cx.ty.usize, span);
            }
            if self.cx.ty.array_elem(t).is_some() {
                self.array_used_in_place(&obj);
                return self.intrinsic(Intrinsic::ArrayLen, vec![obj], self.cx.ty.usize, span);
            }
        }
        // `s.f` on a `shared<T>` of an object type with a field or getter `f`: the value
        // inside's (#36). A getter is a call on it, refused where it may modify it, as a method
        // is (`ownership/patch.rs`).
        let shared = match self.cx.ty.kind(obj.ty) {
            TyKind::Shared(t) => Some(*t),
            _ => None,
        };
        let obj = match shared {
            Some(t) if self.field_of(t, &prop.name).is_some() || self.has_getter(t, &prop.name) => {
                self.frozen_view(obj, Some(&prop.name))
            }
            _ => obj,
        };
        if self.cx.frozen_reads && self.frozen_buffer_view(&obj, &prop.name, span) {
            return self.error_expr(span);
        }
        let obj = self.error_union_accessor(obj, &prop.name);
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
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        if optional {
            return self.optional_chain(object, span, |s, v| {
                if want != Want::BorrowMut {
                    s.array_used_in_place(&v);
                }
                s.index_of(v, index, want, span)
            });
        }
        if let Some(h) = self.process_env_index(object, index, span) {
            return h;
        }
        if let Some(h) = self.keyed_read(object, index, want, span) {
            return h;
        }
        if let Some(h) = self.generic_key_index(object, index, want, span) {
            return h;
        }
        let obj = self.expr(object, None, Want::Borrow);
        if want != Want::BorrowMut {
            self.array_used_in_place(&obj);
        }
        let r = self.in_place_receiver(object, obj);
        let h = match self.checked_array_read(r.recv, index, exp, want, span) {
            Ok(h) => h,
            Err(recv) => self.index_of(recv, index, want, span),
        };
        let h = self.after_receiver(r.before, h);
        // `o["a"]` narrows like `o.a`: after `if (o["a"] !== null)` or `if (o.a !== null)`.
        match self.single_key(index) {
            Some(name) => {
                let prop = ast::Ident {
                    name,
                    span: index.span,
                };
                let h = self.narrowed_field(object, &prop, h, want);
                self.downcast_field(object, &prop, h)
            }
            None => h,
        }
    }

    /// Does `o[name]` on a value of type `t` name a field (an object type, struct, interface,
    /// union or type parameter, possibly nullable or shared; a class only for one of its fields,
    /// getters or setters, so `m["k"]` on a `Map` keeps the indexing error)?
    fn has_fields(&mut self, t: TyId, name: &str) -> bool {
        if self.cx.class_of(t).is_some() {
            return crate::reserved_key(name)
                || self.field_of(t, name).is_some()
                || self.has_getter(t, name)
                || self.has_setter(t, name);
        }
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(..) | TyKind::Dyn(..) | TyKind::Param(_) => true,
            TyKind::Option(inner, _) | TyKind::Shared(inner) => self.has_fields(inner, name),
            _ => false,
        }
    }

    pub(super) fn index_of(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let obj = match self.dynamic_index(obj, index, want, span) {
            Ok(h) => return h,
            Err(obj) => obj,
        };
        let obj = match matches!(self.cx.ty.kind(obj.ty), TyKind::Shared(_)) {
            true => match self.shared_indexed(obj, index, want, span) {
                Ok(obj) => obj,
                Err(e) => return e,
            },
            false => obj,
        };
        let t = obj.ty;
        // `o[KEY]` with a symbol constant: the member the symbol names.
        if let Some(key) = self.symbol_key_of(index) {
            if self.has_fields(t, &key) {
                return self.symbol_index(obj, key, index, want, span);
            }
        }
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(..) if self.record_args(t).is_some() => {
                self.record_index_read(obj, index, span)
            }
            TyKind::Array(elem) => self.array_index(obj, elem, index, want, span),
            // `m[1]` on a match of `s.matchAll(re)`: its group (std/regex `__index`).
            TyKind::Adt(..) if self.is_std_class(t, "std/regex::RegExpMatch") => {
                let prop = ast::Ident {
                    name: "__index".into(),
                    span,
                };
                self.method_call_on(obj, &prop, &[], std::slice::from_ref(index), None, span)
            }
            // `xs[i]` on a typed array: `xs.__index(i)` (`indexers`).
            TyKind::Adt(..)
                if self.has_indexer(t)
                    && literal_key(index).is_none()
                    && self.symbol_key_of(index).is_none() =>
            {
                self.indexer_read(obj, index, span)
            }
            // `s[i]` is `s.charAt(i)` (a string, as in JS).
            TyKind::Str => {
                let prop = ast::Ident {
                    name: "charAt".into(),
                    span,
                };
                self.method_call_on(obj, &prop, &[], std::slice::from_ref(index), None, span)
            }
            TyKind::Tuple(ts) => self.tuple_index(obj, &ts, index, want, span),
            TyKind::Error | TyKind::Never => {
                self.expr(index, None, Want::Borrow);
                self.error_expr(span)
            }
            // `o["content-type"]`: a constant key names a field, as `o.name` does (JS reads the
            // same property either way; the quoted form allows any name). Only on a type with
            // fields: a `Map` keeps its "use a method" error.
            _ if self
                .single_key(index)
                .is_some_and(|k| self.has_fields(t, &k)) =>
            {
                let prop = ast::Ident {
                    name: self.single_key(index).unwrap_or_default(),
                    span: index.span,
                };
                if literal_key(index).is_none() {
                    // `o[k]` with `const k = "a"`: `k` counts as used.
                    self.expr(index, None, Want::Borrow);
                }
                if crate::reserved_key(&prop.name) {
                    // Not the private field `#x`, a symbol-keyed member or the prototype.
                    self.cx
                        .err(crate::reserved_key_message(&prop.name), index.span);
                    return self.error_expr(span);
                }
                if matches!(want, Want::BorrowMut) {
                    let Some(place) = self.field_access(obj, &prop, want, span) else {
                        return self.error_expr(span);
                    };
                    self.check_readonly(&place, &prop);
                    return place;
                }
                self.member_of(obj, &prop, want, span)
            }
            // `obj(os)[key(ks, 0)]` in a generic body: a key known only per instantiation.
            TyKind::Param(_) if want != Want::BorrowMut && literal_key(index).is_none() => {
                self.param_key_read(obj, index, span)
            }
            _ => {
                self.cannot_index(&obj);
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
        // A float index is a JS number (`xs[i]` with `i: number`), except a quotient: `xs[n / 2]`
        // is almost always a forgotten `Math.trunc`, so it stays an error below.
        if self.cx.ty.is_float(i.ty) && self.float_division_note(&i).is_none() {
            i = self.float_index(i);
        }
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

    pub(crate) fn cast(
        &mut self,
        expr: &ast::Expr,
        ty: &ast::TypeExpr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let target = self.resolve(ty);
        if let Some(h) = self.callable_cast(expr, target, span) {
            return h;
        }
        if let Some(base) = self.cx.brand_base(target) {
            return self.brand_cast(expr, target, base, want, span);
        }
        if !self.cx.ty.is_numeric(target) && target != self.cx.ty.bool_ {
            return self.type_assertion(expr, target, want, span);
        }
        // An integer literal cast to an integer type is an exact integer first, so the cast
        // wraps it: `300 as u8` is `44`, `-1 as u8` is `255`.
        let hint = (untyped_int(expr) && self.cx.ty.is_int(target)).then_some(self.cx.ty.i64);
        let inner = self.expr(expr, hint, Want::Borrow);
        if self.cx.dynamic(inner.ty).is_some() {
            return self.cast_dynamic(inner, target);
        }
        // `id as string`: a brand's value as its primitive.
        let inner = match self.cx.brand_base(inner.ty) == Some(target) {
            true => return self.unbrand(inner),
            false => inner,
        };
        // A literal type casts as its base type (`k as f64` with `k: 1 | 2`).
        let inner = self.widen_value(inner);
        // An integer from the standard library made a number (`xs.length as usize`,
        // `s.charCodeAt(i) as i64`) converts from the integer itself: the same value, since
        // such numbers are below 2^53, without a round trip through `f64`.
        let inner = match inner.kind {
            H::Cast(x) if self.cx.ty.is_int(target) && self.is_std_api_value(&x) => *x,
            kind => hir::Expr { kind, ..inner },
        };
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

    /// `e as T` for a `T` that is not a number or `bool`: a TypeScript type assertion, which
    /// converts nothing. `e` is typed in the context of `T`, as for an annotation, so a literal
    /// takes the asserted shape (`[["a", 1]] as [string, u8][]`), and must then be a `T`. A
    /// literal type (or a union with one, `"c" as "a" | "c" | null`) takes a value that converts
    /// to it as it is; a wider value (a `string` as `"a" | "b"`) would need a test.
    fn type_assertion(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let inner = self.expr(expr, Some(target), want);
        if self.cx.dynamic(inner.ty).is_some() {
            if let Ok(h) = self.coerce_dynamic(inner.clone(), target, true) {
                return h;
            }
        }
        // `x as object` of an unbounded `T` (issue #926): checked at run time.
        let coerced = self
            .try_coerce(inner, target)
            .or_else(|h| self.param_as_object(h, target))
            .or_else(|h| self.param_as_object_part(h, target))
            .or_else(|h| self.narrowing_assertion(h, target, span))
            .or_else(|h| self.union_view_cast(h, target, span));
        match coerced {
            Ok(h) => h,
            Err(h) => {
                let nothings = (self.cx.ty.nothing_of(h.ty), self.cx.ty.nothing_of(target));
                if let (Some(from), Some(to)) = nothings {
                    if !from.within(to) && !self.cx.ty.has_error(h.ty) {
                        // TypeScript keeps the value (`undefined` stays `undefined`); the type
                        // `T | null | undefined` that could hold it comes with phase 1b.
                        let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                        self.cx.error(
                            Diagnostic::error(
                                format!("a cast from `{s}` to `{d}` is not supported yet (#930)"),
                                span,
                            )
                            .with_note(format!(
                                "the value would stay `{}`, which `{d}` cannot hold yet; convert it instead (`x ?? {}`)",
                                from.word(),
                                to.word()
                            )),
                        );
                        return self.error_expr(span);
                    }
                }
                if !self.cx.ty.has_error(h.ty) {
                    let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                    let mut diag = Diagnostic::error(format!("cannot cast `{s}` as `{d}`"), span);
                    if self.cx.has_literal_member(target) {
                        diag = diag.with_note(
                            "a value takes a literal type after a test that narrows it (`if (k === \"a\")`)",
                        );
                    }
                    self.cx.error(diag);
                }
                self.error_expr(span)
            }
        }
    }

    /// `x as UserId`: brands a value of the brand's primitive `base` (or one that converts to
    /// it, such as a literal), keeping the value.
    fn brand_cast(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        base: TyId,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let inner = self.expr(expr, Some(base), want);
        let inner = self.unbrand(inner);
        match self.try_coerce(inner, base) {
            Ok(mut h) => {
                h.ty = target;
                h
            }
            Err(h) => {
                let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                self.cx.error(
                    Diagnostic::error(format!("cannot cast `{s}` as `{d}`"), span).with_note(
                        format!(
                            "`{d}` is a branded `{}`: only a `{}` can be branded",
                            self.cx.display(base),
                            self.cx.display(base)
                        ),
                    ),
                );
                self.error_expr(span)
            }
        }
    }

    /// Enum without payloads (casts to its discriminant).
    pub(crate) fn is_c_like_enum(&self, t: TyId) -> bool {
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

/// The type of `x as const` (the parser's `const` type name).
pub(crate) fn is_as_const(ty: &ast::TypeExpr) -> bool {
    matches!(&ty.kind, ast::TypeExprKind::Named { path, args } if args.is_empty() && path.len() == 1 && path[0].name == "const")
}

/// An integer literal without a suffix, possibly negated or in parentheses (`-1`, `(300)`).
pub(super) fn untyped_int(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Int { suffix: None, .. }) => true,
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        }
        | ast::ExprKind::Paren(expr) => untyped_int(expr),
        _ => false,
    }
}

/// Is `e` a name (in parentheses or not)?
pub(crate) fn is_name(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) => true,
        ast::ExprKind::Paren(inner) => is_name(inner),
        _ => false,
    }
}
