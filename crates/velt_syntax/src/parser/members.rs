//! Members of declaration bodies: modifiers (`private public readonly static async override`,
//! accessors `get` / `set`, `protected` on a constructor only), fields
//! (including optional `name?: T`), methods and constructors of `struct`/`class`/`extend`, and the
//! fields and (optionally defaulted) methods of `interface`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

/// Modifiers seen before a member name.
#[derive(Default)]
pub(super) struct Modifiers {
    readonly: bool,
    is_static: bool,
    is_async: bool,
    is_override: bool,
    is_private: bool,
    /// `public` is the default visibility; accepted for TypeScript familiarity.
    is_public: bool,
    /// Only on a constructor: Velt has no `protected` members.
    is_protected: bool,
    is_getter: bool,
    is_setter: bool,
}

impl Modifiers {
    /// Modifiers other than the visibility of a constructor.
    fn any_but_visibility(&self) -> bool {
        self.readonly
            || self.is_static
            || self.is_async
            || self.is_override
            || self.is_getter
            || self.is_setter
    }

    fn ctor_visibility(&self) -> CtorVisibility {
        if self.is_private {
            CtorVisibility::Private
        } else if self.is_protected {
            CtorVisibility::Protected
        } else {
            CtorVisibility::Public
        }
    }
}

/// One member of a `struct`/`class`/`extend` body.
pub(super) enum Member {
    Field(Field),
    Method(Method),
    /// The constructor, who may call it, and the fields its parameter properties declare.
    Constructor(FnDecl, CtorVisibility, Vec<Field>),
}

impl<'a> Parser<'a> {
    /// Modifiers are only modifiers when another name follows, so a member may be named `static`.
    fn parse_modifiers(&mut self) -> Modifiers {
        let mut m = Modifiers::default();
        // `async [Symbol.asyncDispose]()`: a symbol key also follows a modifier, and so does
        // the `*` of a generator method (`static *items()`).
        while Self::is_name(self.nth(1)) || matches!(self.nth(1), Tok::LBracket | Tok::Star) {
            let flag = match self.cur_kw() {
                Some(Kw::Readonly) => &mut m.readonly,
                Some(Kw::Static) => &mut m.is_static,
                Some(Kw::Async) => &mut m.is_async,
                None if self.at_word("mut") => {
                    self.reject_mut_modifier();
                    continue;
                }
                // `override` is contextual (not in the keyword list), so it arrives as an identifier.
                None if self.at_word("override") => &mut m.is_override,
                None if self.at_word("private") && !m.is_public && !m.is_protected => {
                    &mut m.is_private
                }
                None if self.at_word("public") && !m.is_private && !m.is_protected => {
                    &mut m.is_public
                }
                None if self.at_word("protected") && !m.is_private && !m.is_public => {
                    &mut m.is_protected
                }
                // `get name(` starts a getter; `get(` / `get: T` are a member named `get`.
                None if self.at_word("get") && self.nth(2) == Tok::LParen => &mut m.is_getter,
                None if self.at_word("set") && self.nth(2) == Tok::LParen => &mut m.is_setter,
                _ => break,
            };
            if *flag {
                break;
            }
            *flag = true;
            self.bump();
        }
        m
    }

    /// One `struct`/`class`/`extend` member.
    pub(super) fn parse_member(&mut self) -> PResult<Member> {
        let lo = self.cur_lo();
        let mods = self.parse_modifiers();
        if self.at_kw(Kw::Constructor) && self.nth(1) == Tok::LParen {
            let (ctor, fields) = self.parse_constructor(lo, &mods)?;
            return Ok(Member::Constructor(ctor, mods.ctor_visibility(), fields));
        }
        let star = self.at(Tok::Star).then(|| self.cur_span());
        if star.is_some() {
            self.bump();
        }
        let name = self.parse_member_name()?;
        self.reject_protected(&mods, &name);
        if !self.at_method_start() {
            if let Some(s) = star {
                self.error("`*` marks a generator method: expected `(`", s);
            }
            return Ok(Member::Field(self.parse_field_rest(lo, name, &mods)?));
        }
        if mods.readonly {
            self.error("`readonly` is not allowed on methods", name.span);
        }
        let mut sig = self.parse_sig_rest(lo, name, mods.is_async)?;
        sig.is_generator = star.is_some();
        self.check_accessor(&sig, &mods);
        let body = self.parse_block()?;
        Ok(Member::Method(Method {
            decl: FnDecl { sig, body },
            is_static: mods.is_static,
            is_private: mods.is_private,
            is_getter: mods.is_getter,
            is_setter: mods.is_setter,
            is_override: mods.is_override,
        }))
    }

    /// `protected` exists only on constructors (and constructor parameter properties).
    fn reject_protected(&mut self, mods: &Modifiers, name: &Ident) {
        if mods.is_protected {
            self.error(
                "Velt has no `protected` members: use `private` (this class only) or leave the member public; only a constructor can be `protected`",
                name.span,
            );
        }
    }

    /// Accessor shapes: a getter takes no parameters or type parameters and declares its type;
    /// a setter takes exactly one parameter and declares no return type. Neither is `static`
    /// or `async`.
    fn check_accessor(&mut self, sig: &FnSig, mods: &Modifiers) {
        let span = sig.name.span;
        if mods.is_getter && mods.is_setter {
            self.error("a member cannot be both a getter and a setter", span);
            return;
        }
        let what = match (mods.is_getter, mods.is_setter) {
            (true, _) => "getter",
            (_, true) => "setter",
            _ => return,
        };
        if mods.is_getter && (!sig.params.is_empty() || !sig.generics.is_empty()) {
            self.error("a getter cannot have parameters", span);
        }
        if mods.is_getter && sig.ret.is_none() {
            self.error("a getter must declare its return type", span);
        }
        if mods.is_setter && (sig.params.len() != 1 || !sig.generics.is_empty()) {
            self.error("a setter must have exactly one parameter", span);
        }
        if mods.is_setter && sig.ret.is_some() {
            self.error("a setter cannot have a return type", span);
        }
        if mods.is_static || sig.is_async {
            self.error(format!("a {what} cannot be `static` or `async`"), span);
        }
        if sig.is_generator {
            self.error(format!("a {what} cannot be a generator"), span);
        }
    }

    /// After a member name: `(` or `<` starts a method signature.
    fn at_method_start(&mut self) -> bool {
        self.at(Tok::LParen) || self.at(Tok::Lt)
    }

    /// `[?]: Type [= default] ;` after a field name.
    fn parse_field_rest(&mut self, lo: u32, name: Ident, mods: &Modifiers) -> PResult<Field> {
        let optional = self.eat(Tok::Question);
        if !optional && self.at(Tok::Eq) {
            return self.parse_untyped_field(lo, name, mods);
        }
        if !self.eat(Tok::Colon) {
            self.error_expected(if optional { "`:`" } else { "`:` or `(`" });
            return Err(Fail);
        }
        if mods.is_async || mods.is_override || mods.is_getter || mods.is_setter {
            self.error(
                "`async`, `override`, `get` and `set` are not allowed on fields",
                name.span,
            );
        }
        let ty = self.parse_type()?;
        let default = if self.eat(Tok::Eq) {
            Some(self.parse_assign()?)
        } else {
            None
        };
        let span = self.span_from(lo);
        self.expect_member_end()?;
        Ok(Field {
            name,
            ty,
            default,
            readonly: mods.readonly,
            optional,
            is_private: mods.is_private,
            is_static: mods.is_static,
            span,
        })
    }

    /// `name = init;`: the type comes from the initializer (`field_types`).
    fn parse_untyped_field(&mut self, lo: u32, name: Ident, mods: &Modifiers) -> PResult<Field> {
        self.bump(); // =
        let default = self.parse_assign()?;
        let ty = self.field_type_of(&name, &default).ok_or(Fail)?;
        let span = self.span_from(lo);
        self.expect_member_end()?;
        Ok(Field {
            name,
            ty,
            default: Some(default),
            readonly: mods.readonly,
            optional: false,
            is_private: mods.is_private,
            is_static: mods.is_static,
            span,
        })
    }

    /// A field or interface member ends with `;`, `,` or the closing `}`.
    fn expect_member_end(&mut self) -> PResult<()> {
        if self.eat(Tok::Semi) || self.eat(Tok::Comma) || self.at(Tok::RBrace) {
            return Ok(());
        }
        self.error_expected("`;`");
        Err(Fail)
    }

    fn parse_constructor(&mut self, lo: u32, mods: &Modifiers) -> PResult<(FnDecl, Vec<Field>)> {
        let name = self.take_ident();
        if mods.any_but_visibility() {
            self.error(
                "a constructor can only be `public`, `protected` or `private`",
                name.span,
            );
        }
        let (params, fields) = self.parse_ctor_params()?;
        if self.at(Tok::Colon) {
            let span = self.cur_span();
            self.error("a constructor cannot have a return type", span);
            self.bump();
            self.parse_type()?;
        }
        let throws = self.parse_throws_clause()?;
        let sig = FnSig {
            name,
            generics: vec![],
            params,
            ret: None,
            throws,
            is_async: false,
            is_generator: false,
            span: self.span_from(lo),
        };
        let mut body = self.parse_block()?;
        self.store_param_props(&mut body, &fields);
        Ok((FnDecl { sig, body }, fields))
    }

    /// Interface member: a field (`name[?]: T;`) or a method signature, optionally with a
    /// default body (`[mut] [async] name(): R { ... }`).
    pub(super) fn parse_interface_member(&mut self, decl: &mut InterfaceDecl) -> PResult<()> {
        let lo = self.cur_lo();
        let mods = self.parse_modifiers();
        if self.at(Tok::Star) {
            let span = self.cur_span();
            self.error(
                "interface methods cannot be generators: declare the method's return type (`Generator<T>`), and write `*name()` in the implementing class",
                span,
            );
            self.bump();
        }
        let name = self.parse_member_name()?;
        self.reject_protected(&mods, &name);
        if !self.at_method_start() {
            let field = self.parse_field_rest(lo, name, &mods)?;
            if let Some(default) = &field.default {
                self.error("interface fields cannot have a default value", default.span);
            }
            if mods.is_private || mods.is_static {
                self.error(
                    "interface fields cannot be `private` or `static`",
                    field.name.span,
                );
            }
            decl.fields.push(field);
            return Ok(());
        }
        if mods.readonly || mods.is_static || mods.is_override || mods.is_private {
            self.error(
                "`readonly`, `static`, `override` and `private` are not allowed on interface methods",
                name.span,
            );
        }
        let sig = self.parse_sig_rest(lo, name, mods.is_async)?;
        self.check_accessor(&sig, &mods);
        let body = if self.at(Tok::LBrace) {
            Some(self.parse_block()?)
        } else {
            self.expect_member_end()?;
            None
        };
        decl.methods.push(InterfaceMethod {
            sig,
            body,
            is_getter: mods.is_getter,
            is_setter: mods.is_setter,
        });
        Ok(())
    }
}
