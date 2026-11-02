//! The signature a variant's export is compared by ([`super::variants`]): its declaration as
//! written, after its name, whitespace and comments aside.

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind, ItemKind, Lit, PatternKind};

/// The name an item declares, if one.
pub(super) fn item_name(item: &ast::Item) -> Option<&str> {
    Some(match &item.kind {
        ItemKind::Function(f) => &f.sig.name.name,
        ItemKind::ExternFn(sig) => &sig.name.name,
        ItemKind::Struct(t) | ItemKind::Class(t) => &t.name.name,
        ItemKind::Interface(i) => &i.name.name,
        ItemKind::Enum(e) => &e.name.name,
        ItemKind::TypeAlias(a) => &a.name.name,
        ItemKind::Namespace(n) => &n.name.name,
        ItemKind::Var(v) => match &v.pattern.kind {
            PatternKind::Ident(id) => &id.name,
            _ => return None,
        },
        _ => return None,
    })
}

pub(super) fn name_span(item: &ast::Item) -> Option<Span> {
    Some(match &item.kind {
        ItemKind::Function(f) => f.sig.name.span,
        ItemKind::ExternFn(sig) => sig.name.span,
        ItemKind::Struct(t) | ItemKind::Class(t) => t.name.span,
        ItemKind::Interface(i) => i.name.span,
        ItemKind::Enum(e) => e.name.span,
        ItemKind::TypeAlias(a) => a.name.span,
        ItemKind::Namespace(n) => n.name.span,
        ItemKind::Var(v) => v.pattern.span,
        _ => return None,
    })
}

/// The signature of a declaration ([`variants`](self) says what that is).
pub(super) fn signature(item: &ast::Item, src: &str) -> String {
    let text = |span: Span| normalize(src.get(span.lo as usize..span.hi as usize).unwrap_or(""));
    match &item.kind {
        ItemKind::Function(f) if !f.overloads.is_empty() => f
            .overloads
            .iter()
            .map(|o| fn_sig(o, src, false))
            .collect::<Vec<_>>()
            .join("; "),
        ItemKind::Function(f) => fn_sig(&f.sig, src, false),
        ItemKind::ExternFn(sig) => fn_sig(sig, src, false),
        ItemKind::Struct(t) | ItemKind::Class(t) => {
            let kind = if matches!(item.kind, ItemKind::Struct(_)) {
                "struct"
            } else {
                "class"
            };
            type_sig(kind, t, item, src)
        }
        ItemKind::Var(v) => {
            let ty = match (&v.ty, &v.init) {
                (Some(ty), _) => text(ty.span),
                (None, Some(e)) => match &e.kind {
                    ExprKind::Lit(Lit::Str(_)) => "string".into(),
                    ExprKind::Lit(Lit::Bool(_)) => "boolean".into(),
                    ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) => "number".into(),
                    _ => format!("typeof ({})", text(e.span)),
                },
                (None, None) => "unknown".into(),
            };
            format!("{}: {ty}", v.kind.keyword())
        }
        // The declaration after its name, so a renamed re-export compares by what it is.
        _ => {
            let (kw, name) = match &item.kind {
                ItemKind::Interface(i) => ("interface", i.name.span),
                ItemKind::TypeAlias(a) => ("type", a.name.span),
                ItemKind::Enum(e) => ("enum", e.name.span),
                ItemKind::Namespace(n) => ("namespace", n.name.span),
                _ => ("", item.span),
            };
            if kw.is_empty() {
                return text(item.span)
                    .trim_start_matches("export")
                    .trim_start()
                    .to_string();
            }
            let rest = src
                .get(name.hi as usize..item.span.hi as usize)
                .unwrap_or("");
            format!("{kw} {}", normalize(rest))
        }
    }
}

/// `async function<T>(a: A): R throws E`, after the name (a `declare` function and one with a
/// body have the same signature, and a name exported under another one compares by what it
/// is); `named`: with the name, as a class's members are compared.
fn fn_sig(sig: &ast::FnSig, src: &str, named: bool) -> String {
    let from = if named {
        sig.name.span.lo
    } else {
        sig.name.span.hi
    };
    let text = src.get(from as usize..sig.span.hi as usize).unwrap_or("");
    let text = normalize(text.trim_end().trim_end_matches(';'));
    let star = if sig.is_generator { "*" } else { "" };
    let async_ = if sig.is_async { "async " } else { "" };
    format!("{async_}function{star} {text}")
}

/// A class's or struct's heading and its non-private members, without bodies.
fn type_sig(kind: &str, t: &ast::TypeDecl, item: &ast::Item, src: &str) -> String {
    // After the name: `export { K as C }` exports the same class as `C`.
    let rest = src
        .get(t.name.span.hi as usize..item.span.hi as usize)
        .unwrap_or("");
    let heading = normalize(&rest[..body_start(rest)]);
    let abstract_ = if t.is_abstract { "abstract " } else { "" };
    let mut members = vec![];
    if let (Some(ctor), ast::CtorVisibility::Public) = (&t.constructor, t.ctor_visibility) {
        let params: Vec<String> = ctor
            .sig
            .params
            .iter()
            .map(|p| {
                normalize(
                    src.get(p.span.lo as usize..p.span.hi as usize)
                        .unwrap_or(""),
                )
            })
            .collect();
        members.push(format!("constructor({})", params.join(", ")));
    }
    for f in t.fields.iter().filter(|f| !f.is_private) {
        let ty = normalize(
            src.get(f.ty.span.lo as usize..f.ty.span.hi as usize)
                .unwrap_or(""),
        );
        let flags = format!(
            "{}{}",
            if f.is_static { "static " } else { "" },
            if f.readonly { "readonly " } else { "" }
        );
        let opt = if f.optional { "?" } else { "" };
        members.push(format!("{flags}{}{opt}: {ty}", f.name.name));
    }
    for m in t.methods.iter().filter(|m| !m.is_private) {
        let flags = [
            (m.is_static, "static "),
            (m.is_abstract, "abstract "),
            (m.is_getter, "get "),
            (m.is_setter, "set "),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, s)| *s)
        .collect::<String>();
        let sig = fn_sig(&m.decl.sig, src, true);
        members.push(format!("{flags}{}", sig.trim_start_matches("function ")));
    }
    format!("{abstract_}{kind} {heading} {{ {} }}", members.join("; "))
}

/// Where a class's body starts in `text` (its heading on): the first `{` outside `<…>` and
/// `(…)`, so an object type in a bound (`<T extends { id: i64 }>`) or a type argument
/// (`extends Base<{ a: T }>`) stays part of the heading.
fn body_start(text: &str) -> usize {
    let mut depth = 0usize;
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        let arrow = c == '>' && prev == '=';
        prev = c;
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if arrow => {}
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => return i,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    text.len()
}

/// `text` without comments, and with whitespace only where it separates two words: `a : T`,
/// `a: T` and `a:T` read the same. A separator before a closing bracket goes too (`(a, b,)`).
pub(super) fn normalize(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                space = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                space = true;
            }
            c if c.is_whitespace() => space = true,
            '"' | '\'' | '`' => {
                if space && out.ends_with(word) {
                    out.push(' ');
                }
                space = false;
                out.push(c);
                let mut escaped = false;
                for d in chars.by_ref() {
                    out.push(d);
                    if !escaped && d == c {
                        break;
                    }
                    escaped = !escaped && d == '\\';
                }
            }
            c => {
                if matches!(c, ')' | ']' | '}' | '>') && (out.ends_with(',') || out.ends_with(';'))
                {
                    out.pop();
                }
                if space && word(c) && out.ends_with(word) {
                    out.push(' ');
                }
                space = false;
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_ignores_layout_and_comments() {
        assert_eq!(
            normalize(
                "send(req : Request, /* the body */ opts ?: Options,)\n  : Promise<Response> // x"
            ),
            "send(req:Request,opts?:Options):Promise<Response>"
        );
        assert_eq!(normalize("async   function f"), "async function f");
        assert_eq!(normalize("x: \"a  //b\""), "x:\"a  //b\"");
        assert_ne!(normalize("f(a: i64)"), normalize("f(a: f64)"));
    }
}
