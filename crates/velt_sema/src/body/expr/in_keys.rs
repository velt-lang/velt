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

/// What a key known only at run time may be, from its type (later ones win in a union).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KeyKind {
    /// A string (`string`, string literals).
    Str,
    /// A number, or a string or a number: tested as `String(k)`.
    Converted,
    /// A symbol, or it may be one.
    Symbol,
    /// A type parameter, or it may be one.
    Param,
    /// Neither a string, a number nor a symbol (TypeScript's TS2322).
    Other,
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

    /// [`Self::in_presence`], with the classes declaring the member whose instances the value
    /// may hold when its type lacks the member or has it optional (`held_declaring`): `k in o`
    /// tests the value's class for those at run time, as Node finds the member on them.
    pub(crate) fn in_test(
        &mut self,
        t: TyId,
        key: &InKey,
        at: Span,
        report: bool,
    ) -> Option<(Presence, Vec<DefId>)> {
        let p = self.in_presence(t, key, at, report)?;
        let held = match p {
            Presence::No | Presence::Optional(_) => self.held_declaring(t, key.name()),
            Presence::Yes | Presence::Entry => vec![],
        };
        if p == Presence::No {
            if let Some(other) = self.other_holder_declaring(t, key.name()) {
                if report {
                    let (tn, name) = (self.cx.display(t), key.name());
                    self.cx.error(
                        Diagnostic::error(
                            format!("cannot tell whether a `{tn}` has \"{name}\" while compiling: it may hold a `{other}`, which declares it"),
                            at,
                        )
                        .with_note(format!("`{other}` implements the interface and is not a class, so its values cannot be told apart at run time; declare \"{name}\" in the interface (optional: `{name}?: T`)")),
                    );
                }
                return None;
            }
        }
        Some((p, held))
    }

    /// A type of the program that is not a class, implements interface type `t` and declares
    /// member `name` (its name): an interface value may hold one, and only classes can be
    /// told apart at run time.
    fn other_holder_declaring(&mut self, t: TyId, name: &str) -> Option<String> {
        let TyKind::Dyn(i, _) = self.cx.ty.kind(t).clone() else {
            return None;
        };
        for d in (0..self.cx.info.len() as u32).map(DefId) {
            let Some(a) = self.cx.adt(d) else { continue };
            if a.kind == AdtKind::Class || self.cx.scopes[a.module].is_std {
                continue;
            }
            let (declared, shown) = (
                crate::symbols::has_instance_member(&a.fields, &a.methods, name),
                a.name.clone(),
            );
            if declared && self.implements(d, i) {
                return Some(shown);
            }
        }
        None
    }

    /// The classes of the program declaring (or inheriting) member `name` whose instances a
    /// value of type `t` may hold: subclasses of a class, or classes an interface value may
    /// hold (TypeScript-compiled classes define every field, an optional one too).
    fn held_declaring(&mut self, t: TyId, name: &str) -> Vec<DefId> {
        let classes: Vec<DefId> = match self.cx.ty.kind(t).clone() {
            TyKind::Adt(..) => self.class_subclasses(t).unwrap_or_default(),
            TyKind::Dyn(i, _) => {
                let user_classes: Vec<DefId> = (0..self.cx.info.len() as u32)
                    .map(DefId)
                    .filter(|&d| {
                        self.cx.adt(d).is_some_and(|a| {
                            a.kind == AdtKind::Class && !self.cx.scopes[a.module].is_std
                        })
                    })
                    .collect();
                user_classes
                    .into_iter()
                    .filter(|&d| self.holds_class(d, i))
                    .collect()
            }
            _ => vec![],
        };
        classes
            .into_iter()
            .filter(|&d| declares(self.cx, d, name))
            .collect()
    }

    /// What the type of a key known only at run time is assignable to: a union is judged by
    /// its members (`"a" | "b"`, a `keyof` type, `string | number`).
    fn run_time_key(&mut self, t: TyId) -> KeyKind {
        let members = self.cx.union_members(t).unwrap_or_else(|| vec![t]);
        let mut kind = KeyKind::Str;
        for m in members {
            let k = match self.cx.typeof_tag(m) {
                "string" => KeyKind::Str,
                "number" => KeyKind::Converted,
                "symbol" => KeyKind::Symbol,
                _ if matches!(self.cx.ty.kind(m), TyKind::Param(_)) => KeyKind::Param,
                _ => return KeyKind::Other,
            };
            kind = kind.max(k);
        }
        kind
    }

    /// `k in o` where `k` is known only at run time: on a `Record<string, V>`,
    /// `__recordIn(k, o)` (std/prelude/record.vlt; a key that may be a number as `String(k)`,
    /// as JavaScript converts it); elsewhere an error.
    pub(crate) fn dynamic_key_in(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let key_ty = self.trial_expr(lhs).ty;
        let target = self.trial_expr(rhs).ty;
        let kind = self.run_time_key(key_ty);
        let open = self
            .record_args(target)
            .is_some_and(|(k, _)| k == self.cx.ty.str_);
        if !open || !matches!(kind, KeyKind::Str | KeyKind::Converted) {
            let k = self.expr(lhs, None, Want::Borrow);
            self.expr(rhs, None, Want::Borrow);
            if !self.cx.ty.is_bottom(k.ty) && !self.cx.ty.is_bottom(target) {
                let msg = match kind {
                    KeyKind::Other => format!(
                        "the left operand of `in` must be a string, a number or a symbol, found `{}` (TypeScript's TS2322)",
                        self.cx.display(k.ty)
                    ),
                    KeyKind::Param if open => format!(
                        "`in` with a key whose type is the type parameter `{}` is not supported; give the key the type `string`",
                        self.cx.display(k.ty)
                    ),
                    _ => format!(
                        "`in` with a key known only at run time is not supported on `{}`",
                        self.cx.display(target)
                    ),
                };
                self.cx.error(
                    Diagnostic::error(msg, lhs.span)
                        .with_note("Velt objects have fixed fields, so `in` tests a key known while compiling: a string literal (`\"a\" in o`), a symbol constant, or a private name (`#x in o`); keys known only at run time need a `Record<string, V>` or a `Map`"),
                );
            }
            return self.error_expr(span);
        }
        let syn = Synth::new(span, self.f.locals.len());
        let key = match kind {
            KeyKind::Converted => syn.call(syn.name("String"), vec![lhs.clone()]),
            _ => lhs.clone(),
        };
        let test = syn.call(syn.name("__recordIn"), vec![key, rhs.clone()]);
        self.expr(&test, None, Want::Move)
    }
}

/// Does class `d` declare (or inherit) a field, method or accessor named `name`?
fn declares(cx: &crate::ctx::Ctx, d: DefId, name: &str) -> bool {
    let mut cur = Some(d);
    for _ in 0..64 {
        let Some(a) = cur.and_then(|c| cx.adt(c)) else {
            return false;
        };
        if crate::symbols::has_instance_member(&a.fields, &a.methods, name) {
            return true;
        }
        cur = a.base.and_then(|b| match cx.ty.kind(b) {
            TyKind::Adt(bd, _) => Some(*bd),
            _ => None,
        });
    }
    false
}
