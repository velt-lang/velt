//! Members of interface and object types that need more than a field parser: call and
//! construct signatures, accessors (`get v(): T`) and the end of a member (`;`, `,` or a line
//! break).

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// At a call signature (`(x: T): R`, `<T>(x: T): T`) or a construct signature
    /// (`new (x: T): R`) in an interface or object type: the signature, else `None`. Sema
    /// gives it its type (and reports what it does not support). (In TypeScript `new(…)` there
    /// is always a construct signature; a method named `new` is written `"new"(…)`. In a
    /// class, `new()` is a method.)
    pub(super) fn parse_call_signature(&mut self) -> PResult<Option<CallSig>> {
        let construct = self.at(Tok::Kw(Kw::New)) && matches!(self.nth(1), Tok::LParen | Tok::Lt);
        if !(construct || self.at(Tok::LParen) || self.at(Tok::Lt)) {
            return Ok(None);
        }
        let lo = self.cur_lo();
        if construct {
            self.bump(); // new
        }
        let at = self.cur_span();
        let name = Ident {
            name: String::new(),
            span: velt_common::Span::new(at.file, at.lo, at.lo),
        };
        let sig_lo = self.cur_lo();
        let sig = self.parse_sig_rest(sig_lo, name, false, false)?;
        Ok(Some(CallSig {
            sig,
            construct,
            span: self.span_from(lo),
        }))
    }

    /// The end of an object type's member: `;` or `,` (consumed), or a line break before the
    /// next member (TypeScript's automatic semicolon insertion).
    pub(super) fn eat_type_member_end(&mut self) -> bool {
        self.eat(Tok::Semi) || self.eat(Tok::Comma) || self.line_break_before()
    }

    /// `get v(): T` / `set v(x: T)` in an object type (the cursor at `get` / `set`): the name,
    /// the property's type and whether it is a setter; `None` when no accessor starts here.
    pub(super) fn parse_type_accessor(&mut self, lo: u32) -> PResult<Option<TypeAccessor>> {
        let setter = self.at_word("set");
        // A name (or a quoted or computed key) must follow: `get: () => T` is a field `get`.
        let key_follows =
            Self::is_name(self.nth(1)) || matches!(self.nth(1), Tok::LBracket | Tok::Str(_));
        if !(setter || self.at_word("get")) || !key_follows || !self.at_accessor_name() {
            return Ok(None);
        }
        self.bump(); // get / set
        let name = self.parse_prop_key()?;
        let sig = self.parse_sig_rest(lo, name.clone(), false, false)?;
        let ty = match setter {
            true => sig.params.first().map(|p| p.ty.clone()),
            false => sig.ret.clone(),
        };
        let Some(ty) = ty else {
            let what = match setter {
                true => "a setter in an object type takes the value: `set v(value: T)`",
                false => "a getter in an object type needs its result type: `get v(): T`",
            };
            self.error(what, name.span);
            return Err(Fail);
        };
        let setter_param = match setter {
            true => sig.params.first().map(|p| p.name.clone()),
            false => None,
        };
        Ok(Some(TypeAccessor {
            name,
            ty,
            setter_param,
            span: self.span_from(lo),
        }))
    }

    /// Records accessor `a` of an object type as the property it reads or writes: a getter
    /// alone gives a `readonly` field, a setter (alone or with the getter) a writable one.
    pub(super) fn add_type_accessor(
        &mut self,
        fields: &mut Vec<ObjectTypeField>,
        a: TypeAccessor,
        readonly: bool,
    ) {
        let setter = a.setter_param.is_some();
        let part = TypeFieldAccessor {
            setter_param: a.setter_param,
            ty: a.ty.clone(),
            span: a.span,
        };
        let pair = |f: &ObjectTypeField| f.name.name == a.name.name && !f.accessors.is_empty();
        if let Some(f) = fields.iter_mut().find(|f| pair(f)) {
            f.readonly &= !setter;
            if !setter {
                // The property reads as the getter's type.
                f.ty = a.ty;
            }
            f.accessors.push(part);
            return;
        }
        fields.push(ObjectTypeField {
            name: a.name,
            ty: a.ty,
            optional: false,
            readonly: readonly || !setter,
            span: a.span,
            accessors: vec![part],
        });
    }
}

impl Parser<'_> {
    /// `type Name<T> = …;` as an item: a type alias, or, when the type is an object type with
    /// an accessor (`type T = { get v(): number }`), the interface it spells out, whose getters
    /// and setters object literals and classes can implement (an object type's fields hold
    /// plain values).
    pub(super) fn parse_type_alias_item(&mut self) -> PResult<ItemKind> {
        if !self.at_object_alias() {
            return Ok(ItemKind::TypeAlias(self.parse_type_alias()?));
        }
        let snap = self.snapshot();
        let outer = self.accessor_type.take();
        let alias = self.parse_type_alias()?;
        let found = self.accessor_type;
        self.accessor_type = outer;
        let accessors =
            matches!(alias.ty.kind, TypeExprKind::Object(_)) && found == Some(alias.ty.span.lo);
        if !accessors {
            return Ok(ItemKind::TypeAlias(alias));
        }
        self.restore(snap);
        let lo = self.cur_lo();
        self.bump(); // type
        let name = self.parse_ident()?;
        let generics = self.parse_generic_params()?;
        self.expect(Tok::Eq)?;
        let item = self.parse_interface_body(lo, name, generics, vec![])?;
        if !self.at(Tok::RBrace) && !self.at(Tok::Eof) {
            self.expect_semi()?;
        }
        Ok(item)
    }

    /// At `type Name = {` or `type Name<…> = {`?
    fn at_object_alias(&mut self) -> bool {
        match self.nth(2) {
            Tok::Eq => self.nth(3) == Tok::LBrace,
            Tok::Lt => true,
            _ => false,
        }
    }
}

/// An accessor of an object type (`get v(): T`, `set v(x: T)`), recorded as a field.
pub(super) struct TypeAccessor {
    name: Ident,
    ty: TypeExpr,
    /// A setter's parameter name (`None`: a getter).
    setter_param: Option<Ident>,
    span: velt_common::Span,
}

#[cfg(test)]
mod tests {
    use crate::ast::{ItemKind, TypeExprKind};
    use velt_common::FileId;

    fn items(src: &str) -> Vec<ItemKind> {
        let (module, diags) = crate::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        module.items.into_iter().map(|i| i.kind).collect()
    }

    #[test]
    fn an_alias_of_an_object_type_with_accessors_is_an_interface() {
        let src = "type T = { get v(): number; set v(x: number), label: string };\n\
                   type G<X> = { get item(): X };\n\
                   type N = { get v(): number } | null;\n\
                   type P = { inner: { get b(): string; set c(x: number) } };\n\
                   type F = { get: () => number; set(x: number): void };\n";
        let kinds = items(src);
        let ItemKind::Interface(t) = &kinds[0] else {
            panic!("{:?}", kinds[0]);
        };
        assert_eq!((t.methods.len(), t.fields.len()), (2, 1));
        assert!(matches!(&kinds[1], ItemKind::Interface(g) if g.generics.len() == 1));
        assert!(
            matches!(&kinds[2], ItemKind::TypeAlias(_)),
            "{:?}",
            kinds[2]
        );
        // Written in place, a getter is a `readonly` field and a setter a writable one.
        let ItemKind::TypeAlias(p) = &kinds[3] else {
            panic!("{:?}", kinds[3]);
        };
        let TypeExprKind::Object(outer) = &p.ty.kind else {
            panic!("{:?}", p.ty);
        };
        let TypeExprKind::Object(inner) = &outer[0].ty.kind else {
            panic!("{:?}", outer[0].ty);
        };
        let fields: Vec<(&str, bool)> = (inner.iter())
            .map(|f| (f.name.name.as_str(), f.readonly))
            .collect();
        assert_eq!(fields, [("b", true), ("c", false)]);
        // Members named `get` and `set` are no accessors.
        let ItemKind::TypeAlias(f) = &kinds[4] else {
            panic!("{:?}", kinds[4]);
        };
        assert!(matches!(&f.ty.kind, TypeExprKind::Object(fs) if fs.len() == 2));
    }
}
