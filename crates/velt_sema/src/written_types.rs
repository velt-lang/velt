//! Type expressions as the source writes them, for fixes quoted in diagnostics: a resolved type
//! loses the spelling (`number` and `f64` are one type), so a fix that repeats a declaration
//! prints its written types instead.

use velt_syntax::ast::{self, TypeExpr, TypeExprKind as T};

/// `t` as written (`number[]`, `Map<string, T>`, `(x: A) => B`).
pub(crate) fn written(t: &TypeExpr) -> String {
    match &t.kind {
        T::Named { path, args } => {
            let name: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
            let name = name.join(".");
            match args.is_empty() {
                true => name,
                false => format!("{name}<{}>", list(args, ", ")),
            }
        }
        T::Array(e) => match e.kind {
            T::Union(_) | T::Function { .. } => format!("({})[]", written(e)),
            _ => format!("{}[]", written(e)),
        },
        T::Tuple(ts) => format!("[{}]", list(ts, ", ")),
        T::Function {
            params,
            ret,
            throws,
        } => {
            let ps: Vec<String> = params
                .iter()
                .enumerate()
                .map(|(i, p)| format!("p{i}: {}", written(p)))
                .collect();
            let th = throws
                .as_ref()
                .map_or(String::new(), |e| format!(" throws {}", written(e)));
            format!("({}) => {}{th}", ps.join(", "), written(ret))
        }
        T::Union(ts) => list(ts, " | "),
        T::Literal(l) => literal(l),
        T::Object(fs) => {
            let fs: Vec<String> = fs.iter().map(object_field).collect();
            format!("{{ {} }}", fs.join("; "))
        }
        T::Null => "null".into(),
        T::Void => "void".into(),
    }
}

/// A parameter's type as written: `T` for `x?: T` (parsed as `T | null`).
pub(crate) fn written_param(p: &ast::Param) -> String {
    match (&p.ty.kind, p.optional) {
        (T::Union(ts), true) if matches!(ts.last().map(|t| &t.kind), Some(T::Null)) => {
            list(&ts[..ts.len() - 1], " | ")
        }
        _ => written(&p.ty),
    }
}

fn list(ts: &[TypeExpr], sep: &str) -> String {
    let parts: Vec<String> = ts.iter().map(written).collect();
    parts.join(sep)
}

fn object_field(f: &ast::ObjectTypeField) -> String {
    let readonly = if f.readonly { "readonly " } else { "" };
    let (q, ty) = match (&f.ty.kind, f.optional) {
        (T::Union(ts), true) if matches!(ts.last().map(|t| &t.kind), Some(T::Null)) => {
            ("?", list(&ts[..ts.len() - 1], " | "))
        }
        _ => ("", written(&f.ty)),
    };
    format!("{readonly}{}{q}: {ty}", f.name.name)
}

fn literal(l: &ast::SignedLit) -> String {
    let sign = if l.negative { "-" } else { "" };
    match &l.lit {
        ast::Lit::Int { value, suffix } => {
            format!("{sign}{value}{}", suffix.as_deref().unwrap_or(""))
        }
        ast::Lit::Float { value, suffix } => {
            format!("{sign}{value}{}", suffix.as_deref().unwrap_or(""))
        }
        ast::Lit::Str(s) => format!("{s:?}"),
        ast::Lit::Bool(b) => b.to_string(),
        ast::Lit::Null => "null".into(),
    }
}
