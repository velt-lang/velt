//! Type expressions. The AST drops parentheses in types, so they are re-inserted exactly where the
//! grammar needs them (`(A | B)[]`, `((x: T) => U) | null`, a union in an `extends` clause), and
//! function-type parameter names are recovered from the source. Literal types keep their source
//! spelling; object types are `{ a: T; b: U }`, one field per line when too long.

use velt_syntax::ast::{Lit, ObjectTypeField, SignedLit, TypeExpr, TypeExprKind};

use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, text, Doc};
use crate::source::{fn_type_param_name, literal_tokens};

impl<'a> Printer<'a> {
    /// A type in a position that accepts unions.
    pub(super) fn ty(&mut self, t: &TypeExpr) -> Doc {
        match &t.kind {
            TypeExprKind::Named { path, args } => {
                let path: Vec<&str> = path.iter().map(|s| s.name.as_str()).collect();
                cat![path.join("."), self.type_args(args)]
            }
            TypeExprKind::Array(elem) => cat![self.ty_operand(elem), "[]"],
            TypeExprKind::Tuple(elems) => {
                let docs = elems.iter().map(|e| self.ty(e)).collect();
                cat!["[", join(&text(", "), docs), "]"]
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
            TypeExprKind::Literal(lit) => {
                let spelled = literal_tokens(self.src, t.span);
                text(signed_lit(lit, spelled.first()))
            }
            TypeExprKind::Object(fields) => self.object_type(fields),
            TypeExprKind::Null => "null".into(),
            TypeExprKind::Void => "void".into(),
        }
    }

    /// `{ kind: "circle"; r: f64 }`
    fn object_type(&mut self, fields: &[ObjectTypeField]) -> Doc {
        if fields.is_empty() {
            return "{}".into();
        }
        let docs = fields
            .iter()
            .map(|f| {
                if f.optional {
                    cat![f.name.name.clone(), "?: ", self.ty_optional(&f.ty)]
                } else {
                    cat![f.name.name.clone(), ": ", self.ty(&f.ty)]
                }
            })
            .collect();
        group(cat![
            "{",
            indent(cat![line(), join(&cat![";", line()], docs)]),
            if_break(";", ""),
            line(),
            "}"
        ])
    }

    /// The type of `name?: T` as written: the parser added `| null` (a written `| null` is
    /// redundant there and dropped too).
    pub(super) fn ty_optional(&mut self, t: &TypeExpr) -> Doc {
        let TypeExprKind::Union(members) = &t.kind else {
            return self.ty(t);
        };
        let docs: Vec<Doc> = members
            .iter()
            .filter(|m| !matches!(m.kind, TypeExprKind::Null))
            .map(|m| self.ty_operand(m))
            .collect();
        join(&text(" | "), docs)
    }

    /// A type where a bare union would not parse (`extends`, `implements`, bounds, targets).
    pub(super) fn ty_no_union(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Union(_) => cat!["(", self.ty(t), ")"],
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

    /// Operand of `[]` or `|`: unions and function types need parentheses.
    pub(super) fn ty_operand(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Union(_) | TypeExprKind::Function { .. } => cat!["(", self.ty(t), ")"],
            _ => self.ty(t),
        }
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
