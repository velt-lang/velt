//! `k in o` with a string key (docs/reference/types.md "The `in` operator"). A literal key
//! (`"a" in o`) is answered from `o`'s type while compiling, like a symbol key
//! (`symbol_keys.rs`): a declared member is always there, an optional one is tested at run
//! time, and the members every object inherits from `Object.prototype` (`"toString"`) are
//! always there. A record (`Record<string, V>`) is a dictionary: its keys are tested at run
//! time (`r.__has(k)`), so a deleted key is gone, as in JavaScript. A key known only at run
//! time works on such a record; anywhere else it is an error, since Velt objects have fixed
//! fields and nothing to look a name up in.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::consume::Synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, DefId, TyId, TyKind};
use crate::symbols::Presence;

/// The members of `Object.prototype`, which `k in o` finds on every object (Node 22).
const OBJECT_PROTOTYPE: [&str; 12] = [
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "__proto__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

/// The key `k in o` tests, known while compiling.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum InKey {
    /// A symbol constant or a well-known symbol: the member name it stands for.
    Symbol(String),
    /// A string literal.
    Str(String),
}

impl InKey {
    /// The member name the key stands for.
    pub(crate) fn name(&self) -> &str {
        match self {
            InKey::Symbol(k) | InKey::Str(k) => k,
        }
    }
}

impl FnCx<'_, '_> {
    /// The key the left operand of `in` names, if known while compiling: a symbol key or a
    /// string literal (parenthesized or not).
    pub(crate) fn in_key_of(&mut self, e: &ast::Expr) -> Option<InKey> {
        match &e.kind {
            ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(InKey::Str(s.clone())),
            ast::ExprKind::Paren(inner) => self.in_key_of(inner),
            _ => self.symbol_key_of(e).map(InKey::Symbol),
        }
    }

    /// Whether a (non-null) value of type `t` has member `key`, as `key in o` tests it:
    /// [`Presence::Entry`] for a record's entry. `None` when Velt cannot tell: reported at `at`
    /// if `report`.
    pub(crate) fn in_presence(
        &mut self,
        t: TyId,
        key: &InKey,
        at: Span,
        report: bool,
    ) -> Option<Presence> {
        let name = match key {
            InKey::Symbol(k) => return self.symbol_presence(t, k, at, report),
            InKey::Str(k) => k.as_str(),
        };
        if OBJECT_PROTOTYPE.contains(&name) {
            return Some(Presence::Yes);
        }
        if let Some((k, _)) = self.record_args(t) {
            return match self.record_keys(k) {
                Some(keys) => Some(match keys.iter().any(|s| s == name) {
                    true => Presence::Yes,
                    false => Presence::No,
                }),
                None if k == self.cx.ty.str_ => Some(Presence::Entry),
                None => {
                    if report {
                        self.cx.error(
                            Diagnostic::error(
                                format!("cannot tell whether a `{}` has \"{name}\"", self.cx.display(t)),
                                at,
                            )
                            .with_note("its key type is a type parameter; test the key where the record's key type is known"),
                        );
                    }
                    None
                }
            };
        }
        let declared = self.cx.declared_presence(t, name);
        if declared.is_none() || declared == Some(Presence::No) {
            if let Some(holder) = self.holder_declaring(t, name) {
                if report {
                    let tn = self.cx.display(t);
                    let hn = self
                        .cx
                        .adt(holder)
                        .map_or(String::new(), |a| a.name.clone());
                    self.cx.error(
                        Diagnostic::error(
                            format!("cannot tell whether a `{tn}` has \"{name}\" while compiling: it may hold a `{hn}`, which declares it"),
                            at,
                        )
                        .with_note(format!("`in` on a value whose class is known only at run time is not supported yet; test `instanceof {hn}`")),
                    );
                }
                return None;
            }
        }
        if declared.is_none() && report {
            self.cx.error(
                Diagnostic::error(
                    format!("cannot tell whether a `{}` has \"{name}\"", self.cx.display(t)),
                    at,
                )
                .with_note("`in` with a string key is supported on object types, classes, interfaces and `Record`s; built-in and standard library values are not supported yet"),
            );
        }
        declared
    }

    /// [`Self::key_presence`] for a symbol key, reported at `at` when unknown (if `report`).
    fn symbol_presence(&mut self, t: TyId, key: &str, at: Span, report: bool) -> Option<Presence> {
        if self.record_args(t).is_some() {
            // Records have string keys only.
            return Some(Presence::No);
        }
        let p = self.key_presence(t, key);
        if p.is_none() && report {
            let (shown, tn) = (self.cx.shown_name(key), self.cx.display(t));
            self.cx.error(
                Diagnostic::error(format!("cannot tell whether a `{tn}` has `{shown}`"), at)
                    .with_note("Velt knows the well-known symbols of built-in and standard library types only for `Symbol.iterator` and `Symbol.asyncIterator`"),
            );
        }
        p
    }

    /// A class whose instances a value of type `t` (a class or an interface the program
    /// declares) may hold at run time that declares member `name`, which `t` doesn't: a
    /// subclass, or a class implementing the interface.
    fn holder_declaring(&self, t: TyId, name: &str) -> Option<DefId> {
        let held = |cx: &crate::ctx::Ctx, d: DefId| match cx.ty.kind(t) {
            TyKind::Adt(c, _) => *c != d && extends(cx, d, *c),
            TyKind::Dyn(i, _) => implements(cx, d, *i),
            _ => false,
        };
        (0..self.cx.info.len() as u32).map(DefId).find(|&d| {
            self.cx
                .adt(d)
                .is_some_and(|a| a.kind == AdtKind::Class && !self.cx.scopes[a.module].is_std)
                && held(self.cx, d)
                && declares(self.cx, d, name)
        })
    }

    /// `k in o` where `k` is known only at run time: on a `Record<string, V>`, `{ const <k> =
    /// lhs; o.__has(<k>) || __objectPrototypeKey(<k>) }`; elsewhere an error.
    pub(crate) fn dynamic_key_in(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let target = self.trial_expr(rhs).ty;
        let open = self
            .record_args(target)
            .is_some_and(|(k, _)| k == self.cx.ty.str_);
        if !open {
            let k = self.expr(lhs, None, Want::Borrow);
            self.expr(rhs, None, Want::Borrow);
            if !self.cx.ty.is_bottom(k.ty) && !self.cx.ty.is_bottom(target) {
                let msg = match self.cx.typeof_tag(k.ty) {
                    "string" | "number" | "symbol" => format!(
                        "`in` with a key known only at run time is not supported on `{}`",
                        self.cx.display(target)
                    ),
                    _ => format!(
                        "the left operand of `in` must be a string, a number or a symbol, found `{}` (TypeScript's TS2322)",
                        self.cx.display(k.ty)
                    ),
                };
                self.cx.error(
                    Diagnostic::error(msg, lhs.span)
                        .with_note("Velt objects have fixed fields, so `in` tests a key known while compiling: a string literal (`\"a\" in o`), a symbol constant, or a private name (`#x in o`); keys known only at run time need a `Record<string, V>` or a `Map`"),
                );
            }
            return self.error_expr(span);
        }
        self.push_scope_until(span.hi);
        let syn = Synth::new(span, self.f.locals.len());
        let mut lets = vec![];
        let str_ = self.cx.ty.str_;
        let k = self.expr_coerce(lhs, str_, Want::Move);
        self.hidden_local(syn.ident(&syn.value), k, false, &mut lets);
        // An empty span marks the `__has` call as the compiler's (`record_internal_call`).
        let has = syn.call(syn.member(rhs.clone(), "__has"), vec![syn.name(&syn.value)]);
        let proto = syn.call(syn.name("__objectPrototypeKey"), vec![syn.name(&syn.value)]);
        let test = syn.expr(ast::ExprKind::Binary {
            op: ast::BinaryOp::Or,
            lhs: Box::new(has),
            rhs: Box::new(proto),
        });
        let h = self.expr(&test, None, Want::Move);
        self.pop_scope();
        self.with_lets(lets, h)
    }
}

/// Does class `d` extend class `base`, directly or not?
fn extends(cx: &crate::ctx::Ctx, d: DefId, base: DefId) -> bool {
    let mut cur = Some(d);
    for _ in 0..64 {
        let Some(c) = cur else {
            return false;
        };
        if c == base {
            return true;
        }
        cur = cx
            .adt(c)
            .and_then(|a| a.base)
            .and_then(|b| match cx.ty.kind(b) {
                TyKind::Adt(bd, _) => Some(*bd),
                _ => None,
            });
    }
    false
}

/// Does class `d` (or a base class) implement interface `i` (or an interface extending it)?
fn implements(cx: &crate::ctx::Ctx, d: DefId, i: DefId) -> bool {
    let mut cur = Some(d);
    for _ in 0..64 {
        let Some(a) = cur.and_then(|c| cx.adt(c)) else {
            return false;
        };
        if a.implements
            .iter()
            .any(|b| iface_extends(cx, b.iface, i, 0))
        {
            return true;
        }
        cur = a.base.and_then(|b| match cx.ty.kind(b) {
            TyKind::Adt(bd, _) => Some(*bd),
            _ => None,
        });
    }
    false
}

/// Is interface `j` interface `i`, or does it extend it?
fn iface_extends(cx: &crate::ctx::Ctx, j: DefId, i: DefId, depth: u32) -> bool {
    j == i
        || (depth < 32
            && cx.iface(j).is_some_and(|f| {
                f.parents
                    .iter()
                    .any(|p| iface_extends(cx, p.iface, i, depth + 1))
            }))
}

/// Does class `d` declare (or inherit) a field, method or accessor named `name`?
fn declares(cx: &crate::ctx::Ctx, d: DefId, name: &str) -> bool {
    let mut cur = Some(d);
    for _ in 0..64 {
        let Some(a) = cur.and_then(|c| cx.adt(c)) else {
            return false;
        };
        if a.fields.iter().any(|f| f.name == name) || a.methods.contains_key(name) {
            return true;
        }
        cur = a.base.and_then(|b| match cx.ty.kind(b) {
            TyKind::Adt(bd, _) => Some(*bd),
            _ => None,
        });
    }
    false
}
