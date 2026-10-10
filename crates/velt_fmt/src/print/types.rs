//! Type expressions. The AST drops parentheses in types, so they are re-inserted exactly where the
//! grammar needs them (`(A | B)[]`, `((x: T) => U) | null`, a union in an `extends` clause), and
//! function-type parameter names are recovered from the source. Literal types keep their source
//! spelling; object types are `{ a: T; b: U }`, one field per line when too long or when a comment
//! sits inside (each comment stays with its field, as in a class body).

use velt_common::Span;
use velt_syntax::ast::{Ident, Lit, ObjectTypeField, SignedLit, TypeExpr, TypeExprKind};

use super::decls::braced;
use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, nil, text, Doc};
use crate::source::{chars_after, fn_type_param_name, literal_tokens, slice};

impl<'a> Printer<'a> {
    /// A type in a position that accepts unions.
    pub(super) fn ty(&mut self, t: &TypeExpr) -> Doc {
        match &t.kind {
            TypeExprKind::Named { path, args } => {
                // `readonly T[]`, which the parser gives as `ReadonlyArray<T>` named by the
                // `readonly` keyword.
                if let ([p], [elem]) = (path.as_slice(), args.as_slice()) {
                    if p.name == "ReadonlyArray" && slice(self.src, p.span) == "readonly" {
                        return cat!["readonly ", self.ty_operand(elem), "[]"];
                    }
                }
                let path: Vec<&str> = path.iter().map(|s| s.name.as_str()).collect();
                cat![path.join("."), self.type_args(args)]
            }
            TypeExprKind::Array(elem) => cat![self.ty_operand(elem), "[]"],
            TypeExprKind::Tuple(elems) => {
                // Element labels (`[kind: string]`) and `readonly` are not in the AST.
                let docs = elems
                    .iter()
                    .map(|e| {
                        let ty = self.ty(e);
                        match fn_type_param_name(self.src, e.span.lo) {
                            Some(label) => cat![label.to_string(), ": ", ty],
                            None => ty,
                        }
                    })
                    .collect();
                let tuple = cat!["[", join(&text(", "), docs), "]"];
                if is_readonly_op(slice(self.src, t.span)) {
                    cat!["readonly ", tuple]
                } else {
                    tuple
                }
            }
            TypeExprKind::Function {
                params,
                ret,
                throws,
            } => {
                let docs = params.iter().map(|p| self.fn_type_param(p)).collect();
                let throws = self.throws_clause(throws.as_deref());
                cat!["(", join(&text(", "), docs), ") => ", self.ty(ret), throws]
            }
            TypeExprKind::Union(members) => {
                let docs = members.iter().map(|m| self.ty_operand(m)).collect();
                join(&text(" | "), docs)
            }
            TypeExprKind::Intersection(members) => {
                let docs = members.iter().map(|m| self.ty_operand(m)).collect();
                join(&text(" & "), docs)
            }
            TypeExprKind::Indexed { object, key } => {
                cat![self.ty_operand(object), "[", self.ty(key), "]"]
            }
            TypeExprKind::Literal(lit) => {
                let spelled = literal_tokens(self.src, t.span);
                text(signed_lit(lit, spelled.first()))
            }
            TypeExprKind::Object(fields) => self.object_type(fields, t.span),
            TypeExprKind::Null => "null".into(),
            TypeExprKind::Void => "void".into(),
            TypeExprKind::Predicate { param, ty, asserts } => {
                let asserts = if *asserts { "asserts " } else { "" };
                match ty {
                    Some(ty) => cat![asserts, param.name.clone(), " is ", self.ty(ty)],
                    None => cat![asserts, param.name.clone()],
                }
            }
        }
    }

    /// `{ kind: "circle"; r: f64 }`; with comments inside, one field per line like an interface.
    fn object_type(&mut self, fields: &[ObjectTypeField], span: Span) -> Doc {
        if self.comments.any_within(span.lo, span.hi) {
            // Comments before the `{` that nobody printed yet stay before it.
            let before = self.comments.take_before(span.lo);
            let body = self.lines(
                fields,
                span.hi,
                |f| (f.span.lo, f.span.hi),
                |_, _| false,
                |p, f| cat![p.object_type_field(f), ";"],
            );
            return cat![self.leading_doc(&before), braced(body)];
        }
        if fields.is_empty() {
            return "{}".into();
        }
        let docs = fields.iter().map(|f| self.object_type_field(f)).collect();
        group(cat![
            "{",
            indent(cat![line(), join(&cat![";", line()], docs)]),
            if_break(";", ""),
            line(),
            "}"
        ])
    }

    /// `name: T` / `name?: T` / `readonly name: T`
    fn object_type_field(&mut self, f: &ObjectTypeField) -> Doc {
        if let Some(sig) = self.method_sig_type(&f.name, f.optional, &f.ty) {
            return sig;
        }
        let key = self.prop_key(&f.name);
        let name = if f.readonly {
            cat!["readonly ", key]
        } else {
            key
        };
        if f.optional {
            cat![name, "?: ", self.ty_optional_field(&f.ty)]
        } else {
            cat![name, ": ", self.ty(&f.ty)]
        }
    }

    /// The type of a parameter `name?: T` as written: the parser added `| null` (a written
    /// `| null` is redundant there and dropped too).
    pub(super) fn ty_optional(&mut self, t: &TypeExpr) -> Doc {
        self.ty_without_nulls(t, |_| true)
    }

    /// The type of a field `name?: T` as written: without the `| null` the parser added (a
    /// zero-width `null`). A written `| null` stays: `a?: T | null` keeps an absent key apart
    /// from a present `null`, so it is not `a?: T`.
    fn ty_optional_field(&mut self, t: &TypeExpr) -> Doc {
        self.ty_without_nulls(t, |m| m.span.lo == m.span.hi)
    }

    /// Union `t` without its `null` members that `drop` selects.
    fn ty_without_nulls(&mut self, t: &TypeExpr, drop: impl Fn(&TypeExpr) -> bool) -> Doc {
        let TypeExprKind::Union(members) = &t.kind else {
            return self.ty(t);
        };
        let docs: Vec<Doc> = members
            .iter()
            .filter(|m| !(matches!(m.kind, TypeExprKind::Null) && drop(m)))
            .map(|m| self.ty_operand(m))
            .collect();
        join(&text(" | "), docs)
    }

    /// A type where a bare union would not parse (`extends`, `implements`, bounds, targets).
    pub(super) fn ty_no_union(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Union(_) | TypeExprKind::Intersection(_) => cat!["(", self.ty(t), ")"],
            _ => self.ty(t),
        }
    }

    /// The type after `as`: only a trailing `| null` is part of the cast type there.
    pub(super) fn ty_cast(&mut self, t: &TypeExpr) -> Doc {
        match &t.kind {
            TypeExprKind::Union(members)
                if members
                    .iter()
                    .skip(1)
                    .all(|m| matches!(m.kind, TypeExprKind::Null)) =>
            {
                self.ty(t)
            }
            _ => self.ty_no_union(t),
        }
    }

    /// Operand of `[]`, `|` or `&`: unions, intersections and function types get parentheses.
    pub(super) fn ty_operand(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Union(_)
            | TypeExprKind::Intersection(_)
            | TypeExprKind::Function { .. } => cat!["(", self.ty(t), ")"],
            // `(readonly T[])[]`
            TypeExprKind::Named { .. } | TypeExprKind::Tuple(_)
                if is_readonly_op(slice(self.src, t.span)) =>
            {
                cat!["(", self.ty(t), ")"]
            }
            _ => self.ty(t),
        }
    }

    /// A method signature `name(x: T): R` / `name?(x: T): R` (in an object type or
    /// interface), which the parser gives as a field of function type: printed as written when
    /// a `(` (or `?(`) follows the name in the source. `None` for a field.
    pub(super) fn method_sig_type(
        &mut self,
        name: &Ident,
        optional: bool,
        ty: &TypeExpr,
    ) -> Option<Doc> {
        let after = chars_after(self.src, name.span.hi);
        let is_sig = match after {
            (Some('?'), Some('(')) => optional,
            (Some('('), _) => !optional,
            _ => false,
        };
        if !is_sig {
            return None;
        }
        let f = match &ty.kind {
            TypeExprKind::Union(members) if optional => members
                .iter()
                .find(|m| matches!(m.kind, TypeExprKind::Function { .. }))?,
            _ => ty,
        };
        let TypeExprKind::Function {
            params,
            ret,
            throws,
        } = &f.kind
        else {
            return None;
        };
        let docs = params.iter().map(|p| self.fn_type_param(p)).collect();
        let throws = self.throws_clause(throws.as_deref());
        let key = self.prop_key(name);
        let q = if optional { "?" } else { "" };
        // A missing return type is a zero-width `void`.
        let ret = if ret.span.lo == ret.span.hi {
            nil()
        } else {
            cat![": ", self.ty(ret)]
        };
        Some(cat![key, q, "(", join(&text(", "), docs), ")", ret, throws])
    }

    fn fn_type_param(&mut self, t: &TypeExpr) -> Doc {
        let ty = self.ty(t);
        match fn_type_param_name(self.src, t.span.lo) {
            Some(name) => cat![name.to_string(), ": ", ty],
            None => ty,
        }
    }
}

/// A literal type: its source spelling when found, else rebuilt from the value.
fn signed_lit(lit: &SignedLit, spelled: Option<&String>) -> String {
    let sign = if lit.negative { "-" } else { "" };
    let body = match spelled {
        Some(s) => s.clone(),
        None => match &lit.lit {
            Lit::Int { value, suffix } => format!("{value}{}", suffix.as_deref().unwrap_or("")),
            Lit::Float { value, suffix } => {
                format!("{value:?}{}", suffix.as_deref().unwrap_or(""))
            }
            Lit::Str(s) => format!("{s:?}"),
            Lit::Bool(b) => b.to_string(),
            Lit::Null => "null".to_string(),
        },
    };
    format!("{sign}{body}")
}

/// Does a type's source start with the `readonly` operator (`readonly [A, B]`)?
/// (A parenthesized type's span includes the parentheses.)
fn is_readonly_op(src: &str) -> bool {
    src.trim_start_matches(|c: char| c == '(' || c.is_whitespace())
        .strip_prefix("readonly")
        .is_some_and(|r| r.starts_with(char::is_whitespace))
}
