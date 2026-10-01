//! What can be called at a call site, and its parameter list: the callee is resolved through sema
//! ([`sema_query`]) and the parameters are read from the definition's one-line `detail`
//! (`function f(a: i64, b?: string): R`, `(method) User.greet(): string`,
//! `constructor User(name: string)`, `let f: (x: i64) => string`), so functions from the prelude,
//! imports and `declare` blocks all work the same. Used by signature help and inlay hints.

use velt_sema::ide::{DefKind, DefRef};
use velt_syntax::ast;

use crate::analysis::Analysis;

/// A callable definition's signature.
#[derive(Debug)]
pub struct Signature {
    /// The whole signature line (sema's `detail`).
    pub label: String,
    /// Parameters in order.
    pub params: Vec<Param>,
}

/// One parameter of a [`Signature`].
#[derive(Debug)]
pub struct Param {
    /// Its name (`rest` for `...rest`; empty when the signature does not name it).
    pub name: String,
    /// Byte range of its text (`name: T`) in [`Signature::label`].
    pub lo: usize,
    pub hi: usize,
}

/// The signature to show for a call of `def` (`new` calls pass the class and get its
/// constructor).
pub fn signature_of(analysis: &Analysis, def: &DefRef, is_new: bool) -> Option<Signature> {
    if def.kind.is_type() {
        if !is_new {
            return None;
        }
        return parse(&constructor_of(analysis, def)?.detail, false);
    }
    let is_value = matches!(
        def.kind,
        DefKind::Local
            | DefKind::Parameter
            | DefKind::Field
            | DefKind::Constant
            | DefKind::StaticField
            | DefKind::Getter
    );
    parse(&def.detail, is_value)
}

/// The constructor of class/struct `def`: sema's definition at the `constructor` of the type's
/// declaration.
fn constructor_of(analysis: &Analysis, def: &DefRef) -> Option<DefRef> {
    let ide = analysis.ide.as_ref()?;
    let module = analysis.modules.get(def.module)?;
    let ctor = module.ast.items.iter().find_map(|item| match &item.kind {
        ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) if t.name.span == def.span => {
            t.constructor.as_ref()
        }
        _ => None,
    })?;
    let name = ctor.sig.name.span;
    ide.def_at(name.file, (name.lo + name.hi) / 2)
        .filter(|d| d.kind == DefKind::Constructor)
}

/// Parse the parameter list of `detail`. For a value (`let f: (x: i64) => R`) the list is the
/// one of its function type, after the first `: `.
pub fn parse(detail: &str, is_value: bool) -> Option<Signature> {
    let open = if is_value {
        let colon = detail.find(": ")?;
        let rest = &detail[colon + 2..];
        rest.starts_with('(').then_some(colon + 2)?
    } else {
        find_param_list(detail)?
    };
    let params = split_params(detail, open)?;
    Some(Signature {
        label: detail.to_string(),
        params,
    })
}

/// The `(` opening the parameter list: the first one right after a name or a generic list
/// (`f(`, `f<T>(`), outside `<...>`.
fn find_param_list(detail: &str) -> Option<usize> {
    let bytes = detail.as_bytes();
    let mut angle = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => angle += 1,
            b'>' if i > 0 && bytes[i - 1] == b'=' => {}
            b'>' => angle = angle.saturating_sub(1),
            b'(' if angle == 0 && i > 0 => {
                let prev = bytes[i - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' || prev == b'>' {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The parameters of the list opening at `open` (split at top-level commas).
fn split_params(detail: &str, open: usize) -> Option<Vec<Param>> {
    let bytes = detail.as_bytes();
    let mut depth = 0usize;
    let mut params = vec![];
    let mut start = open + 1;
    for i in open + 1..bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if bytes[i - 1] == b'=' => {}
            b']' | b'}' | b'>' => depth = depth.saturating_sub(1),
            b')' if depth == 0 => {
                push_param(detail, start, i, &mut params);
                return Some(params);
            }
            b')' => depth -= 1,
            b',' if depth == 0 => {
                push_param(detail, start, i, &mut params);
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

fn push_param(detail: &str, lo: usize, hi: usize, params: &mut Vec<Param>) {
    let text = &detail[lo..hi];
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return;
    }
    let lo = lo + (text.len() - trimmed.len());
    let hi = lo + trimmed.trim_end().len();
    let name = trimmed
        .trim_start_matches("...")
        .split([':', '?'])
        .next()
        .unwrap_or("")
        .trim();
    let named = trimmed.contains(':') && !name.is_empty();
    params.push(Param {
        name: if named {
            name.to_string()
        } else {
            String::new()
        },
        lo,
        hi,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(detail: &str, is_value: bool) -> Vec<String> {
        let sig = parse(detail, is_value).expect("a parameter list");
        sig.params.iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn parses_functions_methods_and_constructors() {
        assert_eq!(
            names("function f<T>(a: T, b?: Map<string, i64>): R", false),
            ["a", "b"]
        );
        assert_eq!(
            names("(method) User.greet(): string", false),
            [] as [&str; 0]
        );
        assert_eq!(names("constructor User(name: string)", false), ["name"]);
        assert_eq!(
            names("(static) Math.max(...values: f64[]): f64", false),
            ["values"]
        );
    }

    #[test]
    fn nested_function_types_do_not_split() {
        let sig = parse(
            "function map(f: (x: i64, y: i64) => i64, n: i64): void",
            false,
        )
        .unwrap();
        assert_eq!(sig.params.len(), 2);
        assert_eq!(
            &sig.label[sig.params[0].lo..sig.params[0].hi],
            "f: (x: i64, y: i64) => i64"
        );
    }

    #[test]
    fn values_use_their_function_type() {
        assert_eq!(names("let cb: (x: i64) => string", true), ["x"]);
        assert!(parse("let n: i64", true).is_none());
    }
}
