//! Type expressions. The AST drops parentheses in types, so they are re-inserted exactly where the
//! grammar needs them (`(A | B)[]`, `((x: T) => U) | null`, a union in an `extends` clause), and
//! function-type parameter names are recovered from the source. Literal types keep their source
//! spelling; object types are `{ a: T; b: U }`, one field per line when too long or when a comment
//! sits inside (each comment stays with its field, as in a class body).

use velt_common::Span;
use velt_syntax::ast::{
    CallSig, Ident, IndexKey, Lit, MappedModifier, ObjectTypeField, SignedLit, TypeExpr,
    TypeExprKind,
};

use super::decls::braced;
use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, nil, text, Doc};
use crate::source::{chars_after, fn_type_param_name, literal_tokens, optional_label, slice};

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
                rest,
            } => {
                let docs = self.fn_type_params(params, *rest);
                let throws = self.throws_clause(throws.as_deref());
                cat!["(", join(&text(", "), docs), ") => ", self.ty(ret), throws]
            }
            TypeExprKind::GenericFunction { type_params, ty } => {
                cat![self.generic_params(type_params), self.ty(ty)]
            }
            TypeExprKind::Construct(ty) => cat!["new ", self.ty(ty)],
            TypeExprKind::TupleRest(ty) => match fn_type_param_name(self.src, ty.span.lo) {
                Some(label) => cat!["...", label.to_string(), ": ", self.ty(ty)],
                None => cat!["...", self.ty_operand(ty)],
            },
            TypeExprKind::TupleOptional(ty) => match optional_label(self.src, ty.span.lo) {
                Some(label) => cat![label.to_string(), "?: ", self.ty(ty)],
                None => cat![self.ty_operand(ty), "?"],
            },
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
            TypeExprKind::IndexSig {
                fields,
                key,
                value,
                readonly,
            } => {
                let members = index_sig_members(fields, key, value, *readonly);
                self.index_sig_type(&members, t.span)
            }
            TypeExprKind::Callable { sigs, members } => {
                let members = callable_members(sigs, members);
                self.index_sig_type(&members, t.span)
            }
            TypeExprKind::Query(path) => {
                let path: Vec<&str> = path.iter().map(|s| s.name.as_str()).collect();
                text(format!("typeof {}", path.join(".")))
            }
            TypeExprKind::KeyOf(inner) => cat!["keyof ", self.ty_operand(inner)],
            TypeExprKind::Conditional(c) => {
                let check = self.ty_cond_operand(&c.check);
                let extends = self.ty_cond_operand(&c.extends);
                let then = self.ty(&c.then);
                let else_ = self.ty(&c.else_);
                cat![check, " extends ", extends, " ? ", then, " : ", else_]
            }
            TypeExprKind::Infer(name) => cat!["infer ", name.name.clone()],
            TypeExprKind::Mapped(m) => {
                let sign = |m: MappedModifier, word: &'static str| match m {
                    MappedModifier::Keep => "",
                    MappedModifier::Add => word,
                    MappedModifier::Remove => match word {
                        "readonly " => "-readonly ",
                        _ => "-?",
                    },
                };
                let readonly = sign(m.readonly, "readonly ");
                let optional = sign(m.optional, "?");
                let name = match &m.name {
                    Some(n) => cat![" as ", self.ty(n)],
                    None => nil(),
                };
                let source = self.ty(&m.source);
                let value = self.ty(&m.value);
                cat![
                    "{ ",
                    readonly,
                    "[",
                    m.param.name.clone(),
                    " in ",
                    source,
                    name,
                    "]",
                    optional,
                    ": ",
                    value,
                    " }"
                ]
            }
            // Kept as written: the quasis' escapes are in the source only.
            TypeExprKind::TemplateLiteral { .. } => text(slice(self.src, t.span).to_string()),
            TypeExprKind::Null => "null".into(),
            TypeExprKind::Undefined => "undefined".into(),
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

    /// `{ a?: T; [key: string]: V }`, like an object type (`object_type`).
    fn index_sig_type(&mut self, members: &[IndexSigMember], span: Span) -> Doc {
        if self.comments.any_within(span.lo, span.hi) {
            let before = self.comments.take_before(span.lo);
            let body = self.index_sig_lines(members, span.hi);
            return cat![self.leading_doc(&before), braced(body)];
        }
        let docs = members.iter().map(|m| self.index_sig_member(m)).collect();
        group(cat![
            "{",
            indent(cat![line(), join(&cat![";", line()], docs)]),
            if_break(";", ""),
            line(),
            "}"
        ])
    }

    /// The members of an object type or interface with an index signature, one per line.
    pub(super) fn index_sig_lines(&mut self, members: &[IndexSigMember], end: u32) -> Doc {
        self.lines(
            members,
            end,
            |m| m.range(),
            |_, _| false,
            |p, m| cat![p.index_sig_member(m), ";"],
        )
    }

    fn index_sig_member(&mut self, m: &IndexSigMember) -> Doc {
        match m {
            IndexSigMember::Field(f) => self.object_type_field(f),
            IndexSigMember::Sig(s) => self.call_sig(s),
            IndexSigMember::Index {
                key,
                value,
                readonly,
            } => {
                let readonly = if *readonly { "readonly " } else { "" };
                cat![
                    readonly,
                    "[",
                    key.name.name.clone(),
                    ": ",
                    self.ty(&key.ty),
                    "]: ",
                    self.ty(value)
                ]
            }
        }
    }

    /// A call signature `<T>(x: T): R` or a construct signature `new (x: T): C`.
    pub(super) fn call_sig(&mut self, s: &CallSig) -> Doc {
        let new = if s.construct { "new " } else { "" };
        cat![new, self.fn_sig(&s.sig, "", s.span.hi)]
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

    /// The type of a parameter `name?: T` as written: without the `| undefined` the parser
    /// added (a zero-width `undefined`). A written `| null` stays: `p?: T | null` is
    /// `T | null | undefined`, not `p?: T`.
    pub(super) fn ty_optional(&mut self, t: &TypeExpr) -> Doc {
        self.ty_optional_field(t)
    }

    /// The type of a field `name?: T` as written: without the `| undefined` the parser added (a
    /// zero-width `undefined`).
    fn ty_optional_field(&mut self, t: &TypeExpr) -> Doc {
        let TypeExprKind::Union(members) = &t.kind else {
            return self.ty(t);
        };
        let kept: Vec<&TypeExpr> = members
            .iter()
            .filter(|m| !(matches!(m.kind, TypeExprKind::Undefined) && m.span.lo == m.span.hi))
            .collect();
        // One member left (`f?: (x: T) => R`): the type as written, without the parentheses
        // it would need as an operand of `|`.
        if let [only] = kept.as_slice() {
            return self.ty(only);
        }
        let docs: Vec<Doc> = kept.iter().map(|m| self.ty_operand(m)).collect();
        join(&text(" | "), docs)
    }

    /// The check or `extends` type of a conditional type: a conditional or function type there
    /// needs parentheses.
    fn ty_cond_operand(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Conditional(_)
            | TypeExprKind::Function { .. }
            | TypeExprKind::GenericFunction { .. }
            | TypeExprKind::Construct(_) => cat!["(", self.ty(t), ")"],
            _ => self.ty(t),
        }
    }

    /// A type where a bare union would not parse (`extends`, `implements`, bounds, targets).
    pub(super) fn ty_no_union(&mut self, t: &TypeExpr) -> Doc {
        match t.kind {
            TypeExprKind::Union(_)
            | TypeExprKind::Intersection(_)
            | TypeExprKind::Conditional(_) => cat!["(", self.ty(t), ")"],
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
            | TypeExprKind::KeyOf(_)
            | TypeExprKind::Conditional(_)
            | TypeExprKind::Infer(_)
            | TypeExprKind::Function { .. }
            | TypeExprKind::GenericFunction { .. }
            | TypeExprKind::Construct(_) => cat!["(", self.ty(t), ")"],
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
            (Some('?'), Some('(' | '<')) => optional,
            (Some('(' | '<'), _) => !optional,
            _ => false,
        };
        if !is_sig {
            return None;
        }
        let f = match &ty.kind {
            TypeExprKind::Union(members) if optional => members.iter().find(|m| {
                matches!(
                    m.kind,
                    TypeExprKind::Function { .. } | TypeExprKind::GenericFunction { .. }
                )
            })?,
            _ => ty,
        };
        // `m<T>(x: T): T`: a field of generic function type.
        let (generics, f) = match &f.kind {
            TypeExprKind::GenericFunction { type_params, ty } => {
                (self.generic_params(type_params), &**ty)
            }
            _ => (nil(), f),
        };
        let TypeExprKind::Function {
            params,
            ret,
            throws,
            rest,
            ..
        } = &f.kind
        else {
            return None;
        };
        let docs = self.fn_type_params(params, *rest);
        let throws = self.throws_clause(throws.as_deref());
        let key = self.prop_key(name);
        let q = if optional { "?" } else { "" };
        // A missing return type is a zero-width `void`.
        let ret = if ret.span.lo == ret.span.hi {
            nil()
        } else {
            cat![": ", self.ty(ret)]
        };
        Some(cat![
            key,
            q,
            generics,
            "(",
            join(&text(", "), docs),
            ")",
            ret,
            throws
        ])
    }

    /// A function type's parameters, the last one a rest parameter (`...xs: T[]`) when `rest`.
    fn fn_type_params(&mut self, params: &[TypeExpr], rest: bool) -> Vec<Doc> {
        let n = params.len();
        params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let doc = self.fn_type_param(p);
                if rest && i + 1 == n {
                    cat!["...", doc]
                } else {
                    doc
                }
            })
            .collect()
    }

    fn fn_type_param(&mut self, t: &TypeExpr) -> Doc {
        if let Some(name) = optional_label(self.src, t.span.lo) {
            return cat![name.to_string(), "?: ", self.ty_optional_field(t)];
        }
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
            Lit::Undefined => "undefined".to_string(),
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

/// A member of an object type with an index signature or call signatures, in source order.
pub(super) enum IndexSigMember<'a> {
    Field(&'a ObjectTypeField),
    Sig(&'a CallSig),
    Index {
        key: &'a IndexKey,
        value: &'a TypeExpr,
        readonly: bool,
    },
}

impl IndexSigMember<'_> {
    fn range(&self) -> (u32, u32) {
        match self {
            IndexSigMember::Field(f) => (f.span.lo, f.span.hi),
            IndexSigMember::Sig(s) => (s.span.lo, s.span.hi),
            IndexSigMember::Index { key, value, .. } => (key.name.span.lo, value.span.hi),
        }
    }
}

/// The named members and the index signature of an object type, in source order.
pub(super) fn index_sig_members<'a>(
    fields: &'a [ObjectTypeField],
    key: &'a IndexKey,
    value: &'a TypeExpr,
    readonly: bool,
) -> Vec<IndexSigMember<'a>> {
    let at = key.name.span.lo;
    let mut out: Vec<IndexSigMember> = fields
        .iter()
        .filter(|f| f.span.lo < at)
        .map(IndexSigMember::Field)
        .collect();
    out.push(IndexSigMember::Index {
        key,
        value,
        readonly,
    });
    out.extend(
        fields
            .iter()
            .filter(|f| f.span.lo > at)
            .map(IndexSigMember::Field),
    );
    out
}

/// The members of an object type with call signatures `sigs` and other members `members` (an
/// object type or one with an index signature), in source order.
pub(super) fn callable_members<'a>(
    sigs: &'a [CallSig],
    members: &'a TypeExpr,
) -> Vec<IndexSigMember<'a>> {
    let mut out = match &members.kind {
        TypeExprKind::IndexSig {
            fields,
            key,
            value,
            readonly,
        } => index_sig_members(fields, key, value, *readonly),
        TypeExprKind::Object(fields) => fields.iter().map(IndexSigMember::Field).collect(),
        _ => vec![],
    };
    out.extend(sigs.iter().map(IndexSigMember::Sig));
    out.sort_by_key(|m| m.range().0);
    out
}
