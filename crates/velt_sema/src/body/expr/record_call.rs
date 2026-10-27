//! Calls of the `__` methods of `Record<K, V>` (std/prelude/record.vlt) that the record syntax
//! turns into (see `record.rs`), and the rule that user code cannot call them itself: a record
//! has no methods of its own (`r.size` reads the key "size").
//!
//! The key argument is built here rather than checked as written, because `r.name` names a key
//! by its string: on a record keyed by a string enum, `r.mem` passes the member whose value is
//! "mem" (`Res.Mem`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::want_of;
use super::method::Resolved;
use super::setters::{side_effect_free, synth};
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, TyId, TyKind};

/// `(object, key)` of a record read or assignment target `object[key]` / `object.name`.
#[derive(Clone, Copy)]
pub(super) enum RecordKey<'a> {
    Index(&'a ast::Expr),
    Name(&'a ast::Ident),
    /// The key of `{ [key]: value }` known only at run time: a number is converted to its
    /// string as JavaScript does (`{ [1]: v }` has the key "1"), a symbol is an error.
    Computed(&'a ast::Expr),
}

impl RecordKey<'_> {
    pub(super) fn side_effect_free(&self) -> bool {
        match self {
            RecordKey::Index(e) | RecordKey::Computed(e) => {
                side_effect_free(e) || matches!(e.kind, ast::ExprKind::Lit(_))
            }
            RecordKey::Name(_) => true,
        }
    }
}

/// `(object, key)` of a record assignment target (parentheses removed).
pub(super) fn record_parts(target: &ast::Expr) -> (&ast::Expr, RecordKey<'_>) {
    match &target.kind {
        ast::ExprKind::Paren(inner) => record_parts(inner),
        ast::ExprKind::Index { object, index, .. } => (object, RecordKey::Index(index)),
        ast::ExprKind::Member { object, prop, .. } => (object, RecordKey::Name(prop)),
        _ => panic!("ICE: record target is not an index or member"),
    }
}

/// The fix for calling the record method `name` directly.
fn record_method_fix(name: &str) -> &'static str {
    match name {
        "__get" | "__at" => "read a key with `r[k]` or `r.name`",
        "__set" => "write a key with `r[k] = v` or `r.name = v`",
        "__delete" => "remove a key with `delete r[k]` (on a `Record<string, V>`)",
        "__has" => "test for a key with `r[k] != null` (on a `Record<string, V>`)",
        "__extend" | "__extendMaybe" => "copy entries with a spread: `{ ...r, ...other }`",
        "__size" => "count the keys with `Object.keys(r).length`",
        "__values" => "list the values with `Object.values(r)`",
        "__entries" => "list the entries with `Object.entries(r)`",
        _ => "list the keys with `Object.keys(r)`",
    }
}

impl FnCx<'_, '_> {
    /// `r.__name(...)` written in user code: the record's internal methods are reserved for the
    /// record syntax (and the prelude). Reports and returns `true` if `recv` is such a call.
    pub(super) fn record_internal_call(&mut self, recv: &hir::Expr, prop: &ast::Ident) -> bool {
        // A call the compiler wrote (a `for...in` loop's key test) has an empty span.
        if !prop.name.starts_with("__")
            || prop.span.lo == prop.span.hi
            || self.record_args(recv.ty).is_none()
            || self.cx.scopes[self.module].is_std
        {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                format!(
                    "`{}` is internal to `Record`: a record has no methods of its own",
                    prop.name
                ),
                prop.span,
            )
            .with_note(record_method_fix(&prop.name)),
        );
        true
    }

    /// `obj.method(args)` for a record method without a key parameter.
    pub(super) fn record_call(
        &mut self,
        obj: hir::Expr,
        method: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let m = ast::Ident {
            name: method.to_string(),
            span,
        };
        let obj = self.unview(obj);
        self.method_call_on(obj, &m, &[], args, None, span)
    }

    /// `obj.method(key, rest...)` for a record method whose first parameter is the key `K`.
    pub(super) fn record_call_keyed(
        &mut self,
        obj: hir::Expr,
        method: &str,
        key: RecordKey<'_>,
        rest: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        self.record_call_keyed_with(obj, method, key, rest, None, span)
    }

    /// `obj.__set(key, value)` with an already checked `value`.
    pub(super) fn record_set_checked(
        &mut self,
        obj: hir::Expr,
        key: RecordKey<'_>,
        value: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        self.record_call_keyed_with(obj, "__set", key, &[], Some(value), span)
    }

    /// [`Self::record_call_keyed`], with the parameter after the key given as a `checked`
    /// expression instead of in `rest`.
    fn record_call_keyed_with(
        &mut self,
        obj: hir::Expr,
        method: &str,
        key: RecordKey<'_>,
        rest: &[ast::Expr],
        checked: Option<hir::Expr>,
        span: Span,
    ) -> hir::Expr {
        self.number_record_key(&obj, &key);
        let obj = self.unview(obj);
        let (mut def, mut slots, mut recv_ty, mut vslot) = self.record_method(obj.ty, method);
        let mut c = self.fn_callable(def, format!("method `{method}`"), span);
        let mut kp = c.params.remove(0);
        let mut kty = self.cx.subst_known(kp.ty, &slots);
        // In a program that defines accessors, which a record may hold, every key.
        let keyed = keyed_method(method)
            .filter(|_| self.cx.objects.record_accessors && kty == self.cx.ty.str_)
            .filter(|m| self.resolve_method(obj.ty, m).is_some());
        let key = match keyed {
            Some(keyed) => {
                (def, slots, recv_ty, vslot) = self.record_method(obj.ty, keyed);
                c = self.fn_callable(def, format!("method `{keyed}`"), span);
                kp = c.params.remove(0);
                kty = self.cx.subst_known(kp.ty, &slots);
                let str_ = self.cx.ty.str_;
                let h = match self.symbolic_key(str_, method, &key, want_of(kp.mode)) {
                    Ok((h, _)) => h,
                    Err(Some(h)) => self.string_key_of(str_, h),
                    Err(None) => self.record_key_arg(str_, &key, want_of(kp.mode)),
                };
                self.coerce(h, kty)
            }
            None => match self.symbolic_key(kty, method, &key, want_of(kp.mode)) {
                // A symbol or `string | symbol` key: the method that also reaches the symbol table.
                Ok((h, keyed)) => {
                    (def, slots, recv_ty, vslot) = self.record_method(obj.ty, keyed);
                    c = self.fn_callable(def, format!("method `{keyed}`"), span);
                    kp = c.params.remove(0);
                    kty = self.cx.subst_known(kp.ty, &slots);
                    self.coerce(h, kty)
                }
                Err(Some(h)) => self.string_key_of(kty, h),
                Err(None) => self.record_key_arg(kty, &key, want_of(kp.mode)),
            },
        };
        let vp = checked.as_ref().map(|_| c.params.remove(0));
        let ck = self.check_call(&c, slots, rest, None, span);
        let kty = self.cx.subst(kp.ty, &ck.type_args);
        let key = self.coerce(key, kty);
        let recv = self.receiver(obj, recv_ty, self.this_mode(def), false);
        let mut args = vec![recv, key];
        if let (Some(v), Some(vp)) = (checked, vp) {
            let vty = self.cx.subst(vp.ty, &ck.type_args);
            args.push(self.coerce(v, vty));
        }
        args.extend(ck.args);
        self.call_throws(def, &ck.type_args, ck.ret, span);
        let callee = match vslot {
            Some(slot) => Callee::Virtual { slot },
            None => Callee::Def(def, ck.type_args),
        };
        self.mk(H::Call { callee, args }, ck.ret, span)
    }

    /// The `Record` method `method` as resolved on `ty`.
    #[allow(clippy::type_complexity)] // the parts of `Resolved` a keyed call uses
    fn record_method(
        &mut self,
        ty: TyId,
        method: &str,
    ) -> (hir::DefId, Vec<Option<TyId>>, Option<TyId>, Option<u32>) {
        match self.resolve_method(ty, method) {
            Some(Resolved::Def {
                def,
                slots,
                recv_ty,
                ..
            }) => (def, slots, Some(recv_ty), None),
            Some(Resolved::Virtual { def, slot, slots }) => (def, slots, None, Some(slot)),
            _ => panic!("ICE: `Record` has no method `{method}`"),
        }
    }

    /// The key of a string-keyed record's index `r[e]` (or computed literal key `{ [e]: v }`)
    /// checked once: `Ok` with the method that takes a `string | symbol` key when its type is a
    /// symbol or `string | symbol`; `Err(Some(h))` for another key of a type to be converted
    /// (`string_key_of`); `Err(None)` for a key checked against the key type as written (a
    /// name, a string literal or template, or a key of another record).
    fn symbolic_key(
        &mut self,
        k: TyId,
        method: &str,
        key: &RecordKey<'_>,
        want: Want,
    ) -> Result<(hir::Expr, &'static str), Option<hir::Expr>> {
        let well_known;
        let e = match key {
            RecordKey::Index(e) | RecordKey::Computed(e) => *e,
            // `r[Symbol.iterator]`, which the parser names `[Symbol.iterator]`: the symbol.
            RecordKey::Name(id) => match well_known_symbol(id) {
                Some(w) if k == self.cx.ty.str_ => {
                    well_known = w;
                    &well_known
                }
                _ => return Err(None),
            },
        };
        let plain = matches!(
            e.kind,
            ast::ExprKind::Lit(ast::Lit::Str(_)) | ast::ExprKind::Template { .. }
        );
        if k != self.cx.ty.str_ || plain {
            return Err(None);
        }
        // The methods of std/prelude/global/record_symbols.vlt, which a program loads when it
        // mentions symbols: without them, a symbol key is the type error it was.
        let keyed = keyed_method(method).filter(|m| self.has_record_method(m));
        let h = self.payload_expr(e, None, want);
        let symbolic = self.symbolic_key_type(h.ty);
        match keyed {
            // A run-time symbol as a computed key of a literal stays an error (`computed_key_of`).
            Some(m) if symbolic && matches!(key, RecordKey::Index(_) | RecordKey::Name(_)) => {
                Ok((h, m))
            }
            _ if matches!(key, RecordKey::Computed(_)) => Err(Some(self.computed_key_of(k, h, e))),
            _ => Err(Some(h)),
        }
    }

    /// The key argument of type `k`: an index expression as written; a name as its string, or
    /// on an enum-keyed record as the member with that value.
    fn record_key_arg(&mut self, k: TyId, key: &RecordKey<'_>, want: Want) -> hir::Expr {
        let id = match key {
            RecordKey::Index(e) => return self.index_key_arg(k, e, want),
            RecordKey::Computed(e) => return self.computed_key_arg(k, e, want),
            RecordKey::Name(id) => *id,
        };
        if let Some(h) = self.enum_key(k, id) {
            return h;
        }
        let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(id.name.clone())), id.span);
        self.expr(&lit, Some(k), want)
    }

    /// The key `e` of `r[e]` for a record keyed by `k`: on a `string`-keyed record a number is
    /// converted to its string, as JavaScript converts a property key (`r[1]` is `r["1"]`;
    /// `Record<number, V>` is such a record, `crate::record_keys`).
    pub(super) fn index_key_arg(&mut self, k: TyId, e: &ast::Expr, want: Want) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        let plain = matches!(
            e.kind,
            ast::ExprKind::Lit(ast::Lit::Str(_)) | ast::ExprKind::Template { .. }
        );
        if k != str_ || plain {
            return self.expr(e, Some(k), want);
        }
        let h = self.payload_expr(e, None, want);
        self.string_key_of(k, h)
    }

    /// The checked key `h` of `r[e]` on a record keyed by `k` (`string`): a number converted to
    /// its string.
    pub(super) fn string_key_of(&mut self, k: TyId, h: hir::Expr) -> hir::Expr {
        if self.cx.ty.is_numeric(h.ty) {
            let span = h.span;
            return self.intrinsic(Intrinsic::ToString, vec![h], self.cx.ty.str_, span);
        }
        self.coerce(h, k)
    }

    /// The key `e` of `{ [e]: value }` for a record keyed by `k`: on a `string`-keyed record
    /// a number is converted to its string (as JavaScript converts a property key), and a symbol
    /// is an error, as a symbol made at run time cannot name a member; so is any other type
    /// (TypeScript's TS2464).
    fn computed_key_arg(&mut self, k: TyId, e: &ast::Expr, want: Want) -> hir::Expr {
        if k != self.cx.ty.str_ {
            return self.expr(e, Some(k), want);
        }
        let h = self.payload_expr(e, None, want);
        self.computed_key_of(k, h, e)
    }

    /// The checked key `h` of `{ [e]: value }` (see `computed_key_arg`).
    fn computed_key_of(&mut self, k: TyId, h: hir::Expr, e: &ast::Expr) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        if self.cx.ty.is_numeric(h.ty) {
            let span = h.span;
            return self.intrinsic(Intrinsic::ToString, vec![h], str_, span);
        }
        if matches!(self.cx.ty.kind(h.ty), TyKind::Symbol) {
            match &e.kind {
                ast::ExprKind::Ident(id) => {
                    self.cx.bad_key(&format!("[{}]", id.name), &id.name, e.span);
                }
                _ => self.cx.error(
                    Diagnostic::error(
                        "a symbol made at run time cannot be a key of an object literal",
                        e.span,
                    )
                    .with_note("a computed key names a well-known symbol (`[Symbol.iterator]`), a module constant initialized with `Symbol(\"...\")`, or a string or number"),
                ),
            }
            return self.error_expr(e.span);
        }
        self.try_coerce(h, k).unwrap_or_else(|h| {
            // Reported here, once, rather than as a mismatch with `string`.
            let found = self.cx.display(h.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("a computed key must be a string, number or symbol, found `{found}`"),
                    e.span,
                )
                .with_note("convert it to a string first (`[String(k)]`)"),
            );
            self.error_expr(e.span)
        })
    }

    /// The member of the string enum `k` whose value is `id` (`None`: `k` is not an enum).
    fn enum_key(&mut self, k: TyId, id: &ast::Ident) -> Option<hir::Expr> {
        let TyKind::Adt(d, args) = self.cx.ty.kind(k).clone() else {
            return None;
        };
        if self.cx.union_def(k).is_some() {
            return None;
        }
        let e = self.cx.enum_info(d)?;
        let Some(vi) = e
            .variants
            .iter()
            .position(|v| v.str_value.as_deref() == Some(id.name.as_str()))
        else {
            let kn = self.cx.display(k);
            self.cx
                .err(format!("`{kn}` has no key \"{}\"", id.name), id.span);
            return Some(self.error_expr(id.span));
        };
        let kind = H::Variant {
            def: d,
            type_args: args,
            variant: vi as u32,
            args: vec![],
        };
        Some(self.mk(kind, k, id.span))
    }
}

/// The `Record` method that takes a `string | symbol` key for `method`, if it has one.
pub(super) fn keyed_method(method: &str) -> Option<&'static str> {
    match method {
        "__get" => Some("__getKeyed"),
        "__set" => Some("__setKeyed"),
        "__delete" => Some("__deleteKeyed"),
        "__has" => Some("__hasKeyed"),
        _ => None,
    }
}

/// `Symbol.name` for the member name `[Symbol.name]` the parser gives `o[Symbol.name]`.
fn well_known_symbol(id: &ast::Ident) -> Option<ast::Expr> {
    // A quoted key (`"[Symbol.foo]"`, its span the text and the quotes) is a string.
    if (id.span.hi - id.span.lo) as usize == id.name.len() + 2 {
        return None;
    }
    let name = id.name.strip_prefix("[Symbol.")?.strip_suffix(']')?;
    let ident = |n: &str| ast::Ident {
        name: n.to_string(),
        span: id.span,
    };
    let object = synth(ast::ExprKind::Ident(ident("Symbol")), id.span);
    let kind = ast::ExprKind::Member {
        object: Box::new(object),
        prop: ident(name),
        optional: false,
    };
    Some(synth(kind, id.span))
}

impl FnCx<'_, '_> {
    /// Does the prelude's `Record` have the method `name` (std/prelude/global/record_symbols.vlt
    /// adds the `Keyed` ones when the program loads it)?
    pub(super) fn has_record_method(&mut self, name: &str) -> bool {
        let Some(d) = self.cx.prelude_adt("Record") else {
            return false;
        };
        let str_ = self.cx.ty.str_;
        let rec = self.cx.ty.intern(hir::TyKind::Adt(d, vec![str_, str_]));
        self.resolve_method(rec, name).is_some()
    }
}
