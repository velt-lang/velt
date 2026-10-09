//! Nominal type declarations: `struct`/`class` (members in source order: fields, constructor and
//! methods are separate lists in the AST), `interface`, `extend` and `enum`.

use velt_syntax::ast::{
    CtorVisibility, EnumDecl, ExtendDecl, Field, FnDecl, InterfaceDecl, InterfaceMethod, Method,
    TypeDecl, TypeExpr, Variant,
};

use super::Printer;
use crate::doc::{cat, hardline, indent, join, nil, text, Doc};

/// A member of a declaration body, borrowed from whichever AST list holds it.
enum Member<'d> {
    Field(&'d Field),
    Constructor(&'d FnDecl, CtorVisibility),
    Method(&'d Method),
    InterfaceMethod(&'d InterfaceMethod),
}

impl Member<'_> {
    fn range(&self) -> (u32, u32) {
        match self {
            Member::Field(f) => (f.span.lo, f.span.hi),
            Member::Constructor(c, _) => (c.sig.span.lo, c.body.span.hi),
            Member::Method(m) => (m.decl.sig.span.lo, m.decl.body.span.hi),
            Member::InterfaceMethod(m) => {
                let hi = m.body.as_ref().map_or(m.sig.span.hi, |b| b.span.hi);
                (m.sig.span.lo, hi)
            }
        }
    }
}

impl<'a> Printer<'a> {
    pub(super) fn type_decl(&mut self, keyword: &str, decl: &TypeDecl) -> Doc {
        // Fields declared by constructor parameter properties are printed as the parameters.
        let sig = decl.constructor.as_ref().map(|c| c.sig.span);
        let own = |f: &&Field| sig.is_none_or(|s| f.span.lo < s.lo || f.span.hi > s.hi);
        let mut members: Vec<Member> = decl.fields.iter().filter(own).map(Member::Field).collect();
        let visibility = decl.ctor_visibility;
        members.extend(
            decl.constructor
                .iter()
                .map(|c| Member::Constructor(c, visibility)),
        );
        members.extend(decl.methods.iter().map(Member::Method));
        let mut head = cat![
            keyword,
            decl.name.name.clone(),
            self.generic_params(&decl.generics)
        ];
        if let Some(base) = &decl.extends {
            head = cat![head, " extends ", self.ty_no_union(base)];
        }
        head = cat![head, self.type_list(" implements ", &decl.implements)];
        cat![head, " ", self.members(members, decl.name.span.hi)]
    }

    pub(super) fn interface(&mut self, decl: &InterfaceDecl) -> Doc {
        let mut members: Vec<Member> = decl.fields.iter().map(Member::Field).collect();
        members.extend(decl.methods.iter().map(Member::InterfaceMethod));
        let head = cat![
            "interface ",
            decl.name.name.clone(),
            self.generic_params(&decl.generics),
            self.type_list(" extends ", &decl.extends)
        ];
        cat![head, " ", self.members(members, decl.name.span.hi)]
    }

    pub(super) fn extend(&mut self, decl: &ExtendDecl) -> Doc {
        let members = decl.methods.iter().map(Member::Method).collect();
        let head = cat![
            "extend",
            self.generic_params(&decl.generics),
            " ",
            self.ty_no_union(&decl.target)
        ];
        cat![head, " ", self.members(members, decl.target.span.hi)]
    }

    pub(super) fn enum_decl(&mut self, decl: &EnumDecl) -> Doc {
        let head = cat!["enum ", decl.name.name.clone()];
        let after = decl
            .variants
            .last()
            .map_or(decl.name.span.hi, |v| v.span.hi);
        let end = self.body_end(after);
        let body = self.lines(
            &decl.variants,
            end,
            |v| (v.span.lo, v.span.hi),
            |_, _| false,
            |p, v| p.variant(v),
        );
        cat![head, " ", braced(body)]
    }

    fn variant(&mut self, v: &Variant) -> Doc {
        let mut doc = text(v.name.name.clone());
        if let Some(d) = &v.discriminant {
            doc = cat![doc, " = ", self.expr(d)];
        }
        cat![doc, ","]
    }

    /// ` extends A, B` / ` implements A, B`, or nothing.
    fn type_list(&mut self, keyword: &str, tys: &[TypeExpr]) -> Doc {
        if tys.is_empty() {
            return nil();
        }
        let docs = tys.iter().map(|t| self.ty_no_union(t)).collect();
        cat![keyword, join(&text(", "), docs)]
    }

    /// `{ member; ... }` with members in source order; `head_end` is where the header ends.
    fn members(&mut self, mut members: Vec<Member>, head_end: u32) -> Doc {
        members.sort_by_key(|m| m.range().0);
        let end = self.body_end(members.last().map_or(head_end, |m| m.range().1));
        let body = self.lines(
            &members,
            end,
            Member::range,
            |_, _| false,
            |p, m| p.member(m),
        );
        braced(body)
    }

    /// End of a declaration body: the closing `}` after the last member (or after the header
    /// when empty), found in the source so comments before it stay inside.
    fn body_end(&self, after: u32) -> u32 {
        let from = after as usize;
        let rest = self.src.get(from..).unwrap_or("");
        match closing_brace(rest) {
            Some(i) => (from + i) as u32,
            None => self.src.len() as u32,
        }
    }

    fn member(&mut self, member: &Member) -> Doc {
        match member {
            Member::Field(f) => self.field(f),
            Member::Constructor(c, visibility) => {
                // Without the `this.x = x` stores the parser adds for parameter properties
                // (they sit before the body).
                let mut c = (*c).clone();
                let body_lo = c.body.span.lo;
                c.body.stmts.retain(|s| s.span.lo >= body_lo);
                let mods = match visibility {
                    CtorVisibility::Public => "",
                    CtorVisibility::Protected => "protected ",
                    CtorVisibility::Private => "private ",
                };
                cat![mods, self.fn_decl(&c, "")]
            }
            Member::Method(m) => {
                let mut mods = String::new();
                // A `#m` is private by its name; `private #m` is not valid.
                if m.is_private && !m.decl.sig.name.is_private_name() {
                    mods.push_str("private ");
                }
                if m.is_static {
                    mods.push_str("static ");
                }
                if m.is_override {
                    mods.push_str("override ");
                }
                if m.is_getter {
                    mods.push_str("get ");
                }
                if m.is_setter {
                    mods.push_str("set ");
                }
                cat![mods, self.fn_decl(&m.decl, "")]
            }
            Member::InterfaceMethod(m) => {
                let mut mods = String::new();
                if m.is_getter {
                    mods.push_str("get ");
                }
                if m.is_setter {
                    mods.push_str("set ");
                }
                let asyncness = if m.sig.is_async { "async " } else { "" };
                match &m.body {
                    Some(body) => {
                        let sig = self.fn_sig(&m.sig, "", body.span.lo);
                        cat![mods, asyncness, sig, " ", self.block(body)]
                    }
                    None => {
                        let sig = self.fn_sig(&m.sig, "", m.sig.span.hi);
                        cat![mods, asyncness, sig, ";"]
                    }
                }
            }
        }
    }

    fn field(&mut self, f: &Field) -> Doc {
        let private = if f.is_private && !f.name.is_private_name() {
            "private "
        } else {
            ""
        };
        let is_static = if f.is_static { "static " } else { "" };
        let readonly = if f.readonly { "readonly " } else { "" };
        let optional = if f.optional { "?" } else { "" };
        // `count = 0;`: the parser took the type from the initializer (its span is the
        // initializer's), so none is written.
        let inferred = f.default.as_ref().is_some_and(|d| d.span == f.ty.span);
        let ty = if inferred {
            nil()
        } else {
            cat![": ", self.ty(&f.ty)]
        };
        let lhs = cat![
            private,
            is_static,
            readonly,
            self.prop_key(&f.name),
            optional,
            ty
        ];
        match &f.default {
            Some(d) => cat![self.assignment(lhs, " =", d), ";"],
            None => cat![lhs, ";"],
        }
    }
}

/// `{}` or `{` + indented body + `}`.
pub(super) fn braced(body: Doc) -> Doc {
    if body.is_nil() {
        return text("{}");
    }
    cat!["{", indent(cat![hardline(), body]), hardline(), "}"]
}

/// Offset of the first `}` in `s` outside comments.
fn closing_brace(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'}' => return Some(i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = s[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = s[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            }
            _ => i += 1,
        }
    }
    None
}
