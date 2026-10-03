//! Signatures printed from the syntax tree in one canonical form, whatever the source's
//! layout: `function f<T extends A & B>(x: T, y?: U): R throws E`, `class Box<T> extends Base<T>
//! implements I`, `x: f64`. Spacing, line breaks and trailing commas in the source don't show;
//! optional parameters and fields keep their `?` spelling; parameter defaults, literal types and
//! constant initializers are taken from the source with whitespace collapsed.

use velt_common::Span;
use velt_syntax::ast::{self, GenericParam, TypeExpr, TypeExprKind};

/// Prints signatures of one source file.
pub(crate) struct Printer<'a> {
    pub(crate) src: &'a str,
}

impl Printer<'_> {
    /// `span`'s source text.
    pub(crate) fn slice(&self, span: Span) -> &str {
        self.src
            .get(span.lo as usize..span.hi as usize)
            .unwrap_or("")
    }

    /// `span`'s text with whitespace runs collapsed to one space (except inside string
    /// literals).
    pub(crate) fn flat(&self, span: Span) -> String {
        collapse(self.slice(span))
    }

    /// `name<T, U extends A & B>(params): R throws E` (no keyword or modifiers).
    pub(crate) fn fn_sig(&self, sig: &ast::FnSig) -> String {
        let params: Vec<String> = sig.params.iter().map(|p| self.param(p)).collect();
        let mut out = format!(
            "{}{}({})",
            sig.name.name,
            self.generics(&sig.generics),
            params.join(", ")
        );
        if let Some(ret) = &sig.ret {
            out.push_str(": ");
            out.push_str(&self.ty(ret));
        }
        if let Some(throws) = &sig.throws {
            out.push_str(" throws ");
            out.push_str(&self.ty(throws));
        }
        out
    }

    fn param(&self, p: &ast::Param) -> String {
        // Parameter property modifiers (`private readonly x`) precede the name.
        let mods: String = self
            .src
            .get(p.span.lo as usize..p.name.span.lo.max(p.span.lo) as usize)
            .unwrap_or("")
            .split_whitespace()
            .map(|m| format!("{m} "))
            .collect();
        if p.optional {
            return format!("{mods}{}?: {}", p.name.name, self.ty_optional(&p.ty));
        }
        let mut out = format!("{mods}{}: {}", p.name.name, self.ty(&p.ty));
        if let Some(default) = &p.default {
            out.push_str(" = ");
            out.push_str(&self.flat(default.span));
        }
        out
    }

    /// `<T, U extends A & B>`, or nothing.
    pub(crate) fn generics(&self, generics: &[GenericParam]) -> String {
        if generics.is_empty() {
            return String::new();
        }
        let params: Vec<String> = generics
            .iter()
            .map(|g| {
                if g.bounds.is_empty() {
                    return g.name.name.clone();
                }
                let bounds: Vec<String> = g.bounds.iter().map(|b| self.ty_no_union(b)).collect();
                format!("{} extends {}", g.name.name, bounds.join(" & "))
            })
            .collect();
        format!("<{}>", params.join(", "))
    }

    /// `class Name<T> extends Base implements A, B` (also `struct`, `interface`).
    pub(crate) fn type_header(
        &self,
        keyword: &str,
        name: &str,
        generics: &[GenericParam],
        extends: &[TypeExpr],
        implements: &[TypeExpr],
    ) -> String {
        let mut out = format!("{keyword} {name}{}", self.generics(generics));
        for (word, list) in [("extends", extends), ("implements", implements)] {
            if !list.is_empty() {
                let tys: Vec<String> = list.iter().map(|t| self.ty_no_union(t)).collect();
                out.push_str(&format!(" {word} {}", tys.join(", ")));
            }
        }
        out
    }

    /// A field: `[static ][readonly ]name[?]: T[ = default]`.
    pub(crate) fn field(&self, f: &ast::Field) -> String {
        let mut out = String::new();
        if f.is_static {
            out.push_str("static ");
        }
        if f.readonly {
            out.push_str("readonly ");
        }
        if f.optional {
            out.push_str(&format!("{}?: {}", f.name.name, self.ty_optional(&f.ty)));
            return out;
        }
        out.push_str(&format!("{}: {}", f.name.name, self.ty(&f.ty)));
        if let Some(default) = &f.default {
            out.push_str(" = ");
            out.push_str(&self.flat(default.span));
        }
        out
    }

    /// A type in a position that accepts unions.
    pub(crate) fn ty(&self, t: &TypeExpr) -> String {
        match &t.kind {
            TypeExprKind::Named { path, args } => {
                let path: Vec<&str> = path.iter().map(|s| s.name.as_str()).collect();
                let mut out = path.join(".");
                if !args.is_empty() {
                    let args: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                    out.push_str(&format!("<{}>", args.join(", ")));
                }
                out
            }
            TypeExprKind::Array(elem) => format!("{}[]", self.ty_operand(elem)),
            TypeExprKind::Tuple(elems) => {
                let elems: Vec<String> = elems.iter().map(|e| self.ty(e)).collect();
                format!("[{}]", elems.join(", "))
            }
            TypeExprKind::Function {
                params,
                ret,
                throws,
            } => {
                let params: Vec<String> = params.iter().map(|p| self.fn_type_param(p)).collect();
                let throws = throws
                    .as_ref()
                    .map_or(String::new(), |t| format!(" throws {}", self.ty(t)));
                format!("({}) => {}{throws}", params.join(", "), self.ty(ret))
            }
            TypeExprKind::Union(members) => {
                let members: Vec<String> = members.iter().map(|m| self.ty_operand(m)).collect();
                members.join(" | ")
            }
            TypeExprKind::Literal(_) => self.flat(t.span),
            TypeExprKind::Object(fields) => {
                if fields.is_empty() {
                    return "{}".into();
                }
                let fields: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let readonly = if f.readonly { "readonly " } else { "" };
                        if f.optional {
                            format!("{readonly}{}?: {}", f.name.name, self.ty_optional(&f.ty))
                        } else {
                            format!("{readonly}{}: {}", f.name.name, self.ty(&f.ty))
                        }
                    })
                    .collect();
                format!("{{ {} }}", fields.join("; "))
            }
            TypeExprKind::Null => "null".into(),
            TypeExprKind::Void => "void".into(),
        }
    }

    /// The type of `name?: T` as written: the parser added `| null`.
    fn ty_optional(&self, t: &TypeExpr) -> String {
        let TypeExprKind::Union(members) = &t.kind else {
            return self.ty(t);
        };
        let members: Vec<String> = members
            .iter()
            .filter(|m| !matches!(m.kind, TypeExprKind::Null))
            .map(|m| self.ty_operand(m))
            .collect();
        members.join(" | ")
    }

    /// A type where a bare union would not parse (bounds, `extends`, `implements`, targets).
    pub(crate) fn ty_no_union(&self, t: &TypeExpr) -> String {
        match t.kind {
            TypeExprKind::Union(_) => format!("({})", self.ty(t)),
            _ => self.ty(t),
        }
    }

    /// Operand of `[]` or `|`: unions and function types need parentheses.
    fn ty_operand(&self, t: &TypeExpr) -> String {
        match t.kind {
            TypeExprKind::Union(_) | TypeExprKind::Function { .. } => format!("({})", self.ty(t)),
            _ => self.ty(t),
        }
    }

    /// A function type's parameter, with its name when the source gives one (`x: T`).
    fn fn_type_param(&self, t: &TypeExpr) -> String {
        let ty = self.ty(t);
        match self.param_name_before(t.span.lo) {
            Some(name) => format!("{name}: {ty}"),
            None => ty,
        }
    }

    /// `name` in `name: ` (or `name?: `) right before byte `lo`.
    fn param_name_before(&self, lo: u32) -> Option<&str> {
        let before = self.src.get(..lo as usize)?.trim_end();
        let before = before.strip_suffix(':')?.trim_end();
        let before = before.strip_suffix('?').unwrap_or(before);
        let start = before
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .map_or(0, |i| i + 1);
        let name = &before[start..];
        let preceded_ok = before[..start].trim_end().ends_with(['(', ',']);
        (!name.is_empty() && preceded_ok).then_some(name)
    }
}

/// `text` with whitespace runs outside string literals collapsed to one space, trimmed.
pub(crate) fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut space = false;
    for c in text.chars() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::extract::extract;

    /// Signatures look the same however the source is laid out.
    #[test]
    fn normalizes_layout() {
        let src = "export function   pick< T extends   Comparable<T>  &Clone ,U >(\n  items : T[ ],\n  \
                   f: ( x:T ) =>U,\n  limit?: i64,\n  sep: string = \"  ,\",\n) : Map< string,U > | null throws   Error {\n}\n\
                   export class Box < T > extends Base<T>  implements  Show , Eq {\n  value :T ;\n  \
                   static  readonly  N : i64 = 1;\n  label ?: string;\n  get  size ( ) : i64 { return 0; }\n  \
                   static  of<T>( v : T ): Box<T> { return new Box(); }\n}\n\
                   export type Pair < A,B > = [A ,B];\n\
                   export interface Iter<T> extends Show {\n  next( ): T | null;\n}\n";
        let m = extract("demo", src);
        let sigs: Vec<&str> = m.items.iter().map(|i| i.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "function pick<T extends Comparable<T> & Clone, U>(items: T[], f: (x: T) => U, \
                 limit?: i64, sep: string = \"  ,\"): Map<string, U> | null throws Error",
                "class Box<T> extends Base<T> implements Show, Eq",
                "type Pair<A, B> = [A, B]",
                "interface Iter<T> extends Show",
            ]
        );
        let members: Vec<&str> = m.items[1]
            .members
            .iter()
            .map(|i| i.signature.as_str())
            .collect();
        assert_eq!(
            members,
            [
                "value: T",
                "static readonly N: i64 = 1",
                "label?: string",
                "get size(): i64",
                "static of<T>(v: T): Box<T>",
            ]
        );
        assert_eq!(m.items[3].members[0].signature, "next(): T | null");
    }
}
