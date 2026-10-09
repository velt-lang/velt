//! Member access rules (docs/reference/classes.md):
//! - `private` fields, methods and static fields are usable only inside the body of the type
//!   declaring them (its methods, constructor, field initializers and the closures, functions
//!   and classes declared inside them; not subclasses, like TypeScript). The standard library is one trusted unit: its modules may
//!   use the private members of its own types (one std type builds another's handle, as
//!   `TcpListener.accept()` builds a `TcpStream`), so no std handle can be built or read by user
//!   code.
//! - A `private constructor` is callable (`new C()`) only inside the body of its class (or, for a
//!   std class, anywhere in std, like its other private members); a `protected` one also inside
//!   the bodies of its subclasses (TypeScript's rules; extending a class with a private
//!   constructor is rejected in `crate::collect`). A class without a constructor of its own
//!   inherits its base's, with that constructor's visibility.
//! - `get name(): T` accessors are read as properties: `x.name` is a call of the getter (receiver
//!   borrowed); they cannot be called with `()`, nor assigned unless a setter of the same
//!   name exists (`setters`).
//! - `static readonly NAME` fields are module constants read as `Type.NAME`.
//! - A prelude function `F` whose prelude also exports a class `FConstructor` has that class's
//!   static members as its own (`Number(s)` and `Number.isInteger(x)`, like TypeScript's
//!   `NumberConstructor`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method::Resolved;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, TyId};
use crate::ide::record::Target;

impl FnCx<'_, '_> {
    /// Report a use of a private member of `private_to` outside that type's body.
    pub(crate) fn check_private(&mut self, private_to: Option<DefId>, name: &str, span: Span) {
        let Some(owner) = private_to else { return };
        if crate::defs::is_private_key(name) {
            let name = crate::defs::key_member_name(name).to_string();
            return self.check_private_name(owner, &name, span);
        }
        if self.private_allowed(owner) {
            return;
        }
        let tn = self
            .cx
            .adt(owner)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        self.cx.error(
            Diagnostic::error(format!("`{name}` is private"), span)
                .with_note(format!("it can only be used inside the body of `{tn}`")),
        );
    }

    /// TS18013: an ES private name `#x` declared by class `owner` is usable only in that class's
    /// body (not in subclasses, and with no exemption for std).
    fn check_private_name(&mut self, owner: DefId, name: &str, span: Span) {
        if !self.in_body_of(owner) {
            self.private_name_error(owner, name, "a public getter", span);
        }
    }

    /// TS18013 for `#x` of class `owner`; `instead`: what to add to use it elsewhere.
    fn private_name_error(&mut self, owner: DefId, name: &str, instead: &str, span: Span) {
        let tn = self
            .cx
            .adt(owner)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let plain = name.trim_start_matches(ast::PRIVATE_NAME_PREFIX);
        self.cx.error(
            Diagnostic::error(
                format!("property `{name}` is not accessible outside class `{tn}` because it has a private name"),
                span,
            )
            .with_note(format!(
                "declare it `private {plain}` or add {instead} to use it elsewhere"
            )),
        );
    }

    /// `o.#v` naming a private accessor (`get #v` / `set #v`) of a class whose body this is not
    /// in: reports TS18013 once for the whole read, write or read-modify-write (`write`: it
    /// assigns), and returns true.
    pub(crate) fn private_accessor_outside(
        &mut self,
        t: TyId,
        prop: &ast::Ident,
        write: bool,
    ) -> bool {
        if !prop.name.starts_with(ast::PRIVATE_NAME_PREFIX) {
            return false;
        }
        let setter = crate::defs::member_key(&prop.name, true);
        let owner = [setter.as_str(), prop.name.as_str()]
            .into_iter()
            .find_map(|key| {
                let r = self.resolve_method(t, key)?;
                self.method_private_to(&r)
            });
        let Some(owner) = owner.filter(|&o| !self.in_body_of(o)) else {
            return false;
        };
        let instead = if write {
            "a public setter"
        } else {
            "a public getter"
        };
        self.private_name_error(owner, &prop.name, instead, prop.span);
        true
    }

    /// `new` of a class whose constructor `ctor` is `private` or `protected`, outside the bodies
    /// allowed to call it.
    pub(crate) fn check_ctor_access(&mut self, ctor: DefId, span: Span) {
        let Some(class) = self.cx.fn_info(ctor).owner else {
            return;
        };
        let Some(a) = self.cx.adt(class) else { return };
        let visibility = a.decl.map_or_else(Default::default, |d| d.ctor_visibility);
        if visibility == ast::CtorVisibility::Public {
            return;
        }
        let name = a.name.clone();
        let allowed = self.private_allowed(class)
            || match (self.owner, visibility) {
                (Some(o), ast::CtorVisibility::Protected) => self.cx.class_extends(o, class),
                _ => false,
            };
        if allowed {
            return;
        }
        let message = if visibility == ast::CtorVisibility::Private {
            format!("the constructor of `{name}` is private: only the body of `{name}` can call `new {name}(...)`")
        } else {
            format!("the constructor of `{name}` is protected: only `{name}` and its subclasses can call `new {name}(...)`")
        };
        let note = self.cx.creation_note(class);
        self.cx
            .error(Diagnostic::error(message, span).with_note(note));
    }

    /// May this body use the private members of type `owner`: inside `owner`'s body, or
    /// anywhere in the standard library for a std type.
    pub(crate) fn private_allowed(&self, owner: DefId) -> bool {
        if self.in_body_of(owner) {
            return true;
        }
        let owner_module = self.cx.adt(owner).map(|a| a.module);
        self.cx.scopes[self.module].is_std && owner_module.is_some_and(|m| self.cx.scopes[m].is_std)
    }

    /// The classes (and structs) whose bodies this body is in, innermost first: the type whose
    /// method this is, and for a function or class nested in a method, the types around it
    /// (TypeScript: a nested `function` in a method is inside the class body).
    pub(crate) fn lexical_classes(&self) -> Vec<DefId> {
        let mut out = vec![];
        let mut cur = self.owner.or_else(|| {
            self.body_def
                .and_then(|d| self.cx.enclosing_class.get(&d).copied())
        });
        while let Some(c) = cur {
            if out.contains(&c) {
                break;
            }
            out.push(c);
            cur = self.cx.enclosing_class.get(&c).copied();
        }
        out
    }

    /// Is this body inside the body of class `class`?
    pub(crate) fn in_body_of(&self, class: DefId) -> bool {
        self.owner == Some(class) || self.lexical_classes().contains(&class)
    }

    /// The class whose `#x` a private name `name` in this body means: the innermost enclosing
    /// class declaring it, else the innermost enclosing class. Any other name: the type whose
    /// method this is.
    pub(crate) fn name_owner(&self, name: &str) -> Option<DefId> {
        if !crate::defs::is_private_key(name) {
            return self.owner;
        }
        let classes = self.lexical_classes();
        let member = crate::defs::key_member_name(name);
        classes
            .iter()
            .copied()
            .find(|&c| self.cx.declares_private_name(c, member))
            .or_else(|| classes.first().copied())
    }

    /// `private` check for field `index` of struct/class values of type `t`.
    pub(crate) fn check_field_private(&mut self, t: TyId, index: u32, name: &ast::Ident) {
        let private_to = self.field_private_to(t, index);
        self.check_private(private_to, &name.name, name.span);
    }

    /// The type field `index` of struct/class values of type `t` is private to, if any.
    pub(crate) fn field_private_to(&mut self, t: TyId, index: u32) -> Option<DefId> {
        self.adt_of(t)
            .and_then(|(d, _)| self.cx.adt(d))
            .and_then(|a| a.fields.get(index as usize))
            .and_then(|f| f.private_to)
    }

    /// The type a method is private to, if it is `private`.
    pub(crate) fn fn_private_to(&self, def: DefId) -> Option<DefId> {
        let f = self.cx.fn_info(def);
        f.owner.filter(|_| f.is_private)
    }

    /// The type a resolved method is private to, if it is `private`.
    pub(crate) fn method_private_to(&self, r: &Resolved) -> Option<DefId> {
        match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => self.fn_private_to(*def),
            _ => None,
        }
    }

    /// `span` names the resolved method `r` (for `crate::ide`).
    pub(crate) fn rec_method(&mut self, span: Span, r: &Resolved) {
        let t = match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => Target::Def(*def),
            Resolved::Iface { iface, slot, .. } => Target::IfaceMethod(*iface, *slot),
            Resolved::Builtin(_) => return,
        };
        self.cx.rec_ref(span, t);
    }

    pub(crate) fn is_getter(&self, r: &Resolved) -> bool {
        match r {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => {
                self.cx.fn_info(*def).is_getter
            }
            Resolved::Iface { method, .. } => method.is_getter,
            Resolved::Builtin(_) => false,
        }
    }

    /// Does values of type `t` have a getter `name` (and no field of that name)?
    pub(crate) fn has_getter(&mut self, t: TyId, name: &str) -> bool {
        if self.field_of(t, name).is_some() {
            return false;
        }
        self.resolve_method(t, name)
            .is_some_and(|r| self.is_getter(&r))
    }

    /// `obj.name` where `name` is a getter: the getter's call; `Err` gives `obj` back otherwise.
    pub(crate) fn getter_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.has_getter(obj.ty, &prop.name) {
            return Err(obj);
        }
        Ok(self.method_call_at(obj, prop, &[], &[], None, span, true))
    }

    /// Assigning to a getter without a setter is an error.
    pub(crate) fn reject_getter_assign(&mut self, t: TyId, prop: &ast::Ident) -> bool {
        if !self.has_getter(t, &prop.name) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                format!("cannot assign to `{}`: it is a getter", prop.name),
                prop.span,
            )
            .with_note(format!(
                "define a setter `set {}(v)` to make it assignable",
                prop.name
            )),
        );
        true
    }

    /// The class holding the static members of prelude function `name` (`NumberConstructor`
    /// for `Number`), if `item` is that prelude function and not a user item shadowing it.
    pub(crate) fn companion_class(&self, name: &str, item: crate::ctx::Item) -> Option<DefId> {
        if self.cx.prelude.get(name) != Some(&item) {
            return None;
        }
        self.cx.prelude_adt(&format!("{name}Constructor"))
    }

    /// `Type.NAME` for a `static readonly` field of struct/class `d`.
    pub(crate) fn static_field(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let g = self.cx.adt(d)?.statics.get(&prop.name).copied()?;
        self.cx.rec_ref(prop.span, Target::Def(g));
        let private_to = self
            .cx
            .global(g)
            .filter(|g| g.src.is_private)
            .and_then(|g| g.src.owner);
        self.check_private(private_to, &prop.name, prop.span);
        Some(self.global_read(g, want, span))
    }
}
