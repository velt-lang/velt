//! Abstract members of an `abstract class`: methods and accessors without a body, and abstract
//! properties, which are an abstract getter (and setter, unless `readonly`).

use super::{Member, Modifiers};
use crate::ast::*;
use crate::lexer::Tok;
use crate::parser::{PResult, Parser};

impl<'a> Parser<'a> {
    /// `abstract name(): T;` after the signature: no body (one written is reported and
    /// dropped). The method's body is a `panic` that never runs (`Method::is_abstract`).
    pub(super) fn abstract_method(&mut self, sig: FnSig, mods: &Modifiers) -> PResult<Member> {
        let name = &sig.name;
        let conflict = [
            (mods.is_static, "static"),
            (mods.is_private, "private"),
            (sig.is_async, "async"),
        ]
        .into_iter()
        .find_map(|(on, word)| on.then_some(word));
        if let Some(word) = conflict {
            self.error(
                format!("`{word}` cannot be used with `abstract` (TypeScript's TS1243)"),
                name.span,
            );
        }
        if sig.is_generator {
            self.error("an abstract method cannot be a generator", name.span);
        }
        if !sig.generics.is_empty() {
            self.error(
                "abstract methods cannot have type parameters yet: make the class generic instead",
                name.span,
            );
        }
        if sig.ret.is_none() && mods.is_getter {
            // TypeScript's TS7033 under `noImplicitAny`: there is no body to infer from.
            self.error(
                format!(
                    "abstract getter `{}` must declare its type (`get {}(): T`; TypeScript's TS7033)",
                    name.name, name.name
                ),
                name.span,
            );
        } else if sig.ret.is_none() && !mods.is_setter {
            // TypeScript infers `any` (TS7010 under `noImplicitAny`); there is no body to infer from.
            self.error(
                format!(
                    "abstract method `{}` must declare its return type (`{}(): void` if it returns nothing; TypeScript's TS7010)",
                    name.name, name.name
                ),
                name.span,
            );
        }
        if self.at(Tok::LBrace) {
            let span = self.cur_span();
            self.error(
                format!(
                    "abstract method `{}` cannot have an implementation (TypeScript's TS1245): remove `abstract`, or the body",
                    name.name
                ),
                span,
            );
            self.parse_fn_body()?;
        } else {
            self.expect_member_end()?;
        }
        let body = self.abstract_body(&sig);
        Ok(Member::Method(Method {
            decl: FnDecl {
                sig,
                body,
                overloads: vec![],
            },
            is_static: mods.is_static,
            is_private: mods.is_private,
            is_getter: mods.is_getter,
            is_setter: mods.is_setter,
            // `abstract override m()`: makes an inherited method abstract again.
            is_override: mods.is_override,
            is_abstract: true,
        }))
    }

    /// The placeholder body of the abstract method `sig`: `return panic("…")`. Never run:
    /// only instances of concrete subclasses exist, and those implement the method.
    fn abstract_body(&mut self, sig: &FnSig) -> Block {
        let span = sig.name.span;
        let msg = format!("ICE: abstract method `{}` called", sig.name.name);
        let msg = self.mk_expr(ExprKind::Lit(Lit::Str(msg)), span);
        let callee = self.mk_expr(
            ExprKind::Ident(Ident {
                name: "panic".into(),
                span,
            }),
            span,
        );
        let call = ExprKind::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args: vec![msg],
            optional: false,
        };
        let call = self.mk_expr(call, span);
        Block {
            stmts: vec![Stmt {
                kind: StmtKind::Return(Some(call)),
                span,
            }],
            span,
        }
    }

    /// `abstract [readonly] name[?]: T;` after the name: an abstract getter `get name(): T` and,
    /// unless `readonly`, an abstract setter `set name(v: T)` (left in `abstract_setter` for
    /// the class body to add next). A subclass implements them with a field or accessors.
    pub(super) fn abstract_property(
        &mut self,
        lo: u32,
        name: Ident,
        mods: &Modifiers,
    ) -> PResult<Member> {
        let optional = self.eat(Tok::Question);
        if !self.eat(Tok::Colon) {
            self.error(
                format!(
                    "abstract property `{}` must declare its type (`{}: T`)",
                    name.name, name.name
                ),
                name.span,
            );
            return Err(crate::parser::Fail);
        }
        let mut ty = self.parse_type()?;
        if optional {
            ty = crate::parser::types::or_undefined(ty);
        }
        if self.at(Tok::Eq) {
            let span = self.cur_span();
            self.error(
                format!(
                    "property `{}` cannot have an initializer because it is marked abstract (TypeScript's TS1267)",
                    name.name
                ),
                span,
            );
            self.bump();
            self.parse_assign()?;
        }
        let span = self.span_from(lo);
        self.expect_member_end()?;
        let conflict = [
            (mods.is_static, "static"),
            (mods.is_private, "private"),
            (mods.is_async, "async"),
        ]
        .into_iter()
        .find_map(|(on, word)| on.then_some(word));
        if let Some(word) = conflict {
            self.error(
                format!("`{word}` cannot be used with `abstract` (TypeScript's TS1243)"),
                name.span,
            );
        }
        let sig = |params, ret| FnSig {
            name: name.clone(),
            generics: vec![],
            params,
            ret,
            throws: None,
            is_async: false,
            is_system: false,
            generator: None,
            is_generator: false,
            this_param: None,
            span,
        };
        let accessor = |p: &mut Self, sig: FnSig, setter: bool| Method {
            decl: FnDecl {
                body: p.abstract_body(&sig),
                sig,
                overloads: vec![],
            },
            is_static: false,
            is_private: false,
            is_getter: !setter,
            is_setter: setter,
            is_override: mods.is_override,
            is_abstract: true,
        };
        if !mods.readonly {
            let param = Param {
                name: Ident {
                    name: "value".into(),
                    span: name.span,
                },
                pattern: None,
                ty: ty.clone(),
                default: None,
                optional: false,
                rest: false,
                span: name.span,
            };
            let set = accessor(self, sig(vec![param], None), true);
            self.abstract_setter = Some(set);
        }
        let get = accessor(self, sig(vec![], Some(ty)), false);
        Ok(Member::Method(get))
    }
}
