//! Classes as plain values (#926, #925): `x.constructor` of an `any`, `object` or `Function`
//! `x`, and a class named where an `any`, `unknown`, `object` or `Function` is expected, as in
//! `const ctor = value.constructor; ctor === Set || ctor === MyClass`.
//!
//! A class's object is a function constant of the module state (made on first use, kept in
//! `objects.class_values`, initialized before any module runs, as the construct-signature values
//! of `ctor_values.rs` are), so `===`, `Map` keys and `Set` members see one object per class, and
//! `typeof` is `"function"`. It is a closure of the prelude's `__classObject` (one closure, so one
//! type and one `any` descriptor for every class), which keeps how Node prints it (`[class A]`,
//! `[Function: Map]`) for velt_vir's `format.rs`; a class with static fields or accessors gets
//! `__classObjectStatics`'s, which Node prints with them: printing it is not supported yet.
//! Calling it stops with Node's `TypeError` for a class called
//!
//! `x.constructor` reads an own `constructor` property first, then picks the object of the class
//! `x` holds (a subclass's when it was converted through its base). A value that holds no class
//! instance gets its kind's object, one each: `Object` (an object literal or a record; the
//! prelude class `Object`'s object, so `=== Object` holds), `Array`, `Number` (also what the name
//! `Number` gives where a dynamic value is expected; it converts as `Number(s)` when called),
//! `String` (the prelude class `String`'s), `Boolean` and `Function`. A record made by
//! `Object.create(null)` has no `constructor`: `undefined`. A class declared in a function or a
//! block is one class per run in JavaScript, which one object can't be: naming it as a plain
//! value is an error, and its instances' `constructor` a `TypeError`, both saying it is not
//! supported yet; so is a standard library class JavaScript has no constructor of the same name
//! for.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::{self, AdtKind, DefId, TyId, TyKind};

/// The kinds of values that hold no class instance, in the order `__anyCtor` takes their objects.
const KINDS: [&str; 6] = ["Object", "Array", "Number", "String", "Boolean", "Function"];

impl FnCx<'_, '_> {
    /// Class `d` named where a value of the dynamic type `exp` is expected: its object
    /// (module docs). `None` when `exp` is not dynamic or `d` is not a class.
    pub(super) fn class_object_value(
        &mut self,
        d: DefId,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let exp = exp?;
        self.cx.dynamic(exp)?;
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let name = a.name.clone();
        if self.declared_in_fn(d) {
            self.cx.error(
                Diagnostic::error(
                    format!("class `{name}` declared in a function or a block can't be used as a plain value yet"),
                    span,
                )
                .with_note("JavaScript makes another class each time its declaration runs, which one value can't stand for (#926)")
                .with_note("declare the class at the top level of the module"),
            );
            return Some(self.error_expr(span));
        }
        let h = self.class_object(d, want, span)?;
        Some(self.coerce(h, exp))
    }

    /// Is class `d` declared in a function (not at its module's top level)?
    fn declared_in_fn(&self, d: DefId) -> bool {
        self.cx.nested.iter().any(|n| n.item == Item::Def(d))
    }

    /// `x.constructor` of the dynamic `obj`: the object of the class it holds, or its kind's
    /// (module docs). `Err(obj)` when `obj` is not an `any`, `object` or `Function`.
    pub(crate) fn dynamic_constructor(
        &mut self,
        obj: hir::Expr,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !matches!(
            self.cx.dynamic(obj.ty),
            Some(Dynamic::Any | Dynamic::Object | Dynamic::Function)
        ) {
            return Err(obj);
        }
        let Some(any) = self.any_ty() else {
            return Ok(self.error_expr(span));
        };
        if !self.in_ctor_helper() {
            // The program's own code: the prelude's `__anyConstructor`, whose body holds the
            // dispatch over the program's classes once.
            self.note_dyn_members();
            let s = self.share_if_place(obj);
            let s = self.coerce(s, any);
            return Ok(self.prelude_hir_call("__anyConstructor", vec![s], any, span));
        }
        // A program that reads no `constructor` never calls the helper: no class objects.
        if !self.cx.objects.ctor_reads {
            return Ok(self.dynamic_nothing(hir::Nothing::Undefined, any, span));
        }
        // The kinds' objects, for a value that holds no class instance (`__anyCtor`).
        self.push_scope();
        let mut lets = vec![];
        let mut kinds = vec![];
        for kind in KINDS {
            let Some(h) = self.kind_object(kind, span) else {
                self.pop_scope();
                return Ok(self.error_expr(span));
            };
            let h = self.coerce(h, any);
            let name = format!("<ctor@{}:{kind}>", span.lo);
            self.named_temp(&name, h, &mut lets);
            kinds.push(synth(ast::ExprKind::Ident(ast::Ident { name, span }), span));
        }
        let record = self.cx.prelude_adt("Record");
        let object = kinds[0].clone();
        let h = self.dynamic_class_match_where(
            obj,
            any,
            ("__anyCtor", kinds),
            |_, _| true,
            |lw, c, read| lw.ctor_arm(c, read, (record, &object), any, span),
            span,
        );
        self.pop_scope();
        Ok(self.with_lets(lets, h))
    }

    /// Is this the body of the prelude's `__anyConstructor`, where `x.constructor` is the
    /// dispatch itself?
    fn in_ctor_helper(&self) -> bool {
        self.cx.scopes[self.module].is_std && self.fn_name.ends_with("__anyConstructor")
    }

    /// The `constructor` of an instance of class `c` held by the dynamic `read`, as an `any`;
    /// `record` is the prelude `Record` with the `Object` kind's object.
    fn ctor_arm(
        &mut self,
        c: DefId,
        read: ast::Expr,
        (record, object): (Option<DefId>, &ast::Expr),
        any: TyId,
        span: Span,
    ) -> hir::Expr {
        let name = self.cx.adt(c).map_or(String::new(), |a| a.name.clone());
        let lit = |s: &str| synth(ast::ExprKind::Lit(ast::Lit::Str(s.to_string())), span);
        if Some(c) == record {
            // An own `constructor` key, else `Object`'s; none for `Object.create(null)`'s.
            return self.prelude_fn_call("__anyRecordCtor", vec![read, object.clone()], span);
        }
        if self.declared_in_fn(c) {
            return self.prelude_fn_call("__anyCtorLocal", vec![lit(&name)], span);
        }
        if self.class_ctor_name(c).is_none() {
            return self.prelude_fn_call("__anyCtorUnsupported", vec![lit(&name)], span);
        }
        match self.class_object(c, Want::Move, span) {
            Some(h) => self.coerce(h, any),
            None => self.error_expr(span),
        }
    }

    /// `Number` named where an `any`, `unknown`, `object` or `Function` is expected: the
    /// `Number` kind's object, which `n.constructor` of a number is (module docs). `None` for
    /// another name, or a `Number` of the program's own.
    pub(super) fn kind_name_value(
        &mut self,
        id: &ast::Ident,
        exp: Option<TyId>,
        want: Want,
    ) -> Option<hir::Expr> {
        if id.name != "Number" || self.is_local_name(&id.name) {
            return None;
        }
        let exp = exp?;
        self.cx.dynamic(exp)?;
        let prelude = self.cx.prelude.get("Number").cloned();
        if prelude.is_none() || self.lookup_item(&id.name, id.span) != prelude {
            return None;
        }
        let h = self.kind_object("Number", id.span)?;
        let h = match want {
            Want::Move => h,
            _ => self.share_if_place(h),
        };
        Some(self.coerce(h, exp))
    }

    /// The object of `kind` (module docs): the prelude class's (`Object`, `String`), else a
    /// constant of its own (`Number`'s converts its argument as `Number(s)` does).
    fn kind_object(&mut self, kind: &str, span: Span) -> Option<hir::Expr> {
        let object = self.cx.prelude_adt("Object")?;
        let class = self
            .cx
            .prelude_adt(kind)
            .filter(|d| self.cx.adt(*d).is_some_and(|a| a.kind == AdtKind::Class));
        if let Some(d) = class {
            return self.class_object(d, Want::Move, span);
        }
        // A key of its own beside `Object`'s (whose key has an `any` result): the kind's
        // values' type as the result.
        let any = self.any_ty()?;
        let tag = match kind {
            "Array" => self.cx.ty.array(any),
            "Number" => self.cx.ty.f64,
            "Boolean" => self.cx.ty.bool_,
            _ => self.cx.ty.unit,
        };
        let key = (object, self.cx.ty.fn_ptr(vec![], tag));
        if kind != "Number" {
            let msg = format!("TypeError: calling `{kind}` through the `constructor` of a value is not supported yet (#926)");
            let init =
                class_object_call("__classObject", &format!("[Function: {kind}]"), msg, span);
            return self.ctor_global(key, object, init, None, Want::Move, span);
        }
        // `(s: string, ...m: __ClassMarker<number>[]): any => Number(s)`: callable as `Number`,
        // and its type tells velt_vir to print it as `[Function: Number]` (`format.rs`).
        let marker = self.cx.prelude_adt("__ClassMarker")?;
        let mark = self.cx.ty.intern(TyKind::Adt(marker, vec![tag]));
        let params = vec![self.cx.ty.str_, self.cx.ty.array(mark)];
        let never = self.cx.ty.never;
        let ty = self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret: any,
            throws: never,
            rest: true,
        });
        let ps = vec![
            arrow_param("#a0", false, span),
            arrow_param("#m", true, span),
        ];
        let body = call_ident("Number", vec![ident_expr("#a0", span)], span);
        let arrow = synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params: ps,
                ret: None,
                throws: None,
                body: ast::ArrowBody::Expr(Box::new(body)),
                is_async: false,
            },
            span,
        );
        self.ctor_global(key, object, arrow, Some(ty), Want::Move, span)
    }

    /// A read of class `d`'s object, made on first use (module docs).
    fn class_object(&mut self, d: DefId, want: Want, span: Span) -> Option<hir::Expr> {
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let (module, name, base) = (a.module, a.name.clone(), a.base);
        // Node prints a class's static fields and accessors after its name, which the class
        // object doesn't hold (yet): printing such a class's object stops (`format.rs`).
        let statics = a.decl.is_some_and(|t| {
            t.fields.iter().any(|f| {
                f.is_static
                    && !f.name.name.starts_with('#')
                    && !f.name.name.starts_with("<static block ")
            }) || t.methods.iter().any(|m| {
                m.is_static
                    && (m.is_getter || m.is_setter)
                    && !m.decl.sig.name.name.starts_with('#')
            })
        });
        let std = self.cx.scopes[module].is_std;
        let any = self.any_ty()?;
        let key = (d, self.cx.ty.fn_ptr(vec![], any));
        let short = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
        let shown = short(&name);
        let msg = match std {
            // `Object(v)` and `String(v)` convert in JavaScript.
            true if matches!(shown.as_str(), "Object" | "String") => format!(
                "TypeError: calling `{shown}` through its class object is not supported yet (#926)"
            ),
            true => format!("TypeError: Constructor {shown} requires 'new'"),
            false => {
                format!("TypeError: Class constructor {shown} cannot be invoked without 'new'")
            }
        };
        // How Node prints it.
        let printed = match std {
            true => format!("[Function: {shown}]"),
            false => {
                let base = base
                    .and_then(|b| self.cx.class_of(b))
                    .and_then(|(b, _)| self.cx.adt(b).map(|a| short(&a.name)));
                match base {
                    Some(b) => format!("[class {shown} extends {b}]"),
                    None => format!("[class {shown}]"),
                }
            }
        };
        let maker = match statics && !std {
            true => "__classObjectStatics",
            false => "__classObject",
        };
        let init = class_object_call(maker, &printed, msg, span);
        self.ctor_global(key, d, init, None, want, span)
    }

    /// The constant under `key` in `objects.class_values`, made on first use in the module of
    /// class `owner` with initializer `init` (of type `ty`, when given); read as `want` says.
    fn ctor_global(
        &mut self,
        key: (DefId, TyId),
        owner: DefId,
        init: ast::Expr,
        ty: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if let Some((_, g)) = self.cx.objects.class_values.iter().find(|(k, _)| *k == key) {
            let g = *g;
            return Some(self.global_read(g, want, span));
        }
        let a = self.cx.adt(owner)?;
        let (module, qual, decl) = (a.module, a.qual_name.clone(), a.span);
        let n = self.cx.objects.class_values.len();
        let init = {
            let mut fcx = crate::body::driver::detached(self.cx, module, &[]);
            fcx.fn_name = format!("{qual}#ctor{n}");
            let h = fcx.expr(&init, ty, Want::Move);
            match ty {
                Some(t) => fcx.coerce(h, t),
                None => h,
            }
        };
        let info = GlobalInfo {
            name: format!("{qual}#ctor{n}"),
            qual_name: format!("{qual}#ctor{n}"),
            module,
            span: Span::DUMMY,
            ty: init.ty,
            init: Some(init),
            state: BodyState::Done,
            src: GlobalSrc {
                ann: None,
                init: None,
                span: decl,
                owner: None,
                is_private: false,
            },
            mutable: false,
            stored: true,
        };
        let g = self
            .cx
            .alloc_def(Span::DUMMY, DefInfo::Global(Box::new(info)));
        self.cx.objects.class_values.push((key, g));
        Some(self.global_read(g, want, span))
    }
}

/// `maker(printed, msg)` (the prelude's `__classObject` or `__classObjectStatics`): a class's
/// (or kind's) object, printed as `printed` and stopping with `msg` when called. One closure
/// for all, so one type and one `any` descriptor whatever the number of classes.
fn class_object_call(maker: &str, printed: &str, msg: String, span: Span) -> ast::Expr {
    let lit = |s: String| synth(ast::ExprKind::Lit(ast::Lit::Str(s)), span);
    call_ident(maker, vec![lit(printed.to_string()), lit(msg)], span)
}

fn ident_expr(name: &str, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Ident(ast::Ident {
            name: name.into(),
            span,
        }),
        span,
    )
}

fn arrow_param(name: &str, rest: bool, span: Span) -> ast::ArrowParam {
    ast::ArrowParam {
        name: ast::Ident {
            name: name.into(),
            span,
        },
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest,
    }
}

fn call_ident(f: &str, args: Vec<ast::Expr>, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Call {
            callee: Box::new(ident_expr(f, span)),
            type_args: vec![],
            args,
            optional: false,
        },
        span,
    )
}
