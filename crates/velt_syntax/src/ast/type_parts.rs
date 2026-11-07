//! Parts of type expressions (part of the `ast` contract): index signature keys, object type
//! fields and call signatures.

use velt_common::Span;

use super::{FnSig, Ident, TypeExpr};

/// A call signature `(x: T): R` / `<T>(x: T): T`, or with `construct` a construct signature
/// `new (x: T): C`, in an interface or object type. `sig.name` is empty (zero-width, at the
/// `(` or `<`); a missing return type is `sig.ret: None`.
#[derive(Clone, Debug)]
pub struct CallSig {
    pub sig: FnSig,
    pub construct: bool,
    /// From `new` (or the signature's start) to the end of its return type.
    pub span: Span,
}

/// The key of an index signature, `[name: ty]` (the name is documentation only).
#[derive(Clone, Debug)]
pub struct IndexKey {
    pub name: Ident,
    pub ty: TypeExpr,
}

/// The member name of a template-literal index signature (`` [key: `data-${string}`]: T ``),
/// kept as an optional member so every pass that follows an interface's members (merging,
/// `extends`, generics, namespaces) carries it. A value has no such property: the name only says
/// which keys the type allows, as JSX attributes look them up. Every hole is `${string}`.
pub fn key_pattern_name(key_name: &str, quasis: &[String]) -> String {
    format!("[{key_name}: `{}`]", quasis.join("${string}"))
}

/// The keys member `name` stands for, when it is a template-literal index signature.
pub fn key_pattern(name: &str) -> Option<KeyPattern<'_>> {
    let rest = name.strip_prefix('[')?.strip_suffix("`]")?;
    let (_, key) = rest.split_once(": `")?;
    Some(KeyPattern(key))
}

/// A key pattern's text between the backticks ([`key_pattern`]).
#[derive(Clone, Copy, Debug)]
pub struct KeyPattern<'a>(&'a str);

impl KeyPattern<'_> {
    /// Does `key` fit? A hole takes any text, also none, as TypeScript's `${string}` does.
    pub fn matches(&self, key: &str) -> bool {
        let mut parts = self.0.split("${string}");
        let first = parts.next().unwrap_or("");
        let Some(mut rest) = key.strip_prefix(first) else {
            return false;
        };
        let parts: Vec<&str> = parts.collect();
        let Some((last, middle)) = parts.split_last() else {
            return rest.is_empty();
        };
        for part in middle {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
        rest.ends_with(last)
    }
}

/// `name: T` / `name?: T` / `readonly name: T` in an object type.
#[derive(Clone, Debug)]
pub struct ObjectTypeField {
    pub name: Ident,
    /// For an optional field, the written type plus `| null`.
    pub ty: TypeExpr,
    /// `name?: T` — parsed as `name: T | null`; the flag makes it an optional field of the object
    /// type (left out by `JSON.stringify` while `null`). An object literal may leave out any
    /// `T | null` field of an object type.
    pub optional: bool,
    /// `readonly name: T` — the field can't be assigned (docs/internals/design/shared-models.md).
    pub readonly: bool,
    pub span: Span,
    /// A field written as accessors (`get v(): T`, `set v(x: T)` or both): each as written, for
    /// tools that print the source (`velt fmt`). The field reads as the getter's type (else the
    /// setter's), `readonly` without a setter.
    pub accessors: Vec<TypeFieldAccessor>,
}

/// One accessor of an object type's field (`ObjectTypeField::accessors`).
#[derive(Clone, Debug)]
pub struct TypeFieldAccessor {
    /// `set v(x: T)`: the parameter's name; `None` for a getter.
    pub setter_param: Option<Ident>,
    /// The getter's result type or the setter's parameter type.
    pub ty: TypeExpr,
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::{key_pattern, key_pattern_name};

    fn fits(quasis: &[&str], key: &str) -> bool {
        let quasis: Vec<String> = quasis.iter().map(|q| q.to_string()).collect();
        let name = key_pattern_name("key", &quasis);
        key_pattern(&name).expect("a pattern").matches(key)
    }

    #[test]
    fn a_hole_takes_any_text_also_none() {
        assert!(fits(&["on", ""], "onClick"));
        assert!(fits(&["on", ""], "on"));
        assert!(!fits(&["on", ""], "click"));
        assert!(fits(&["a", "b"], "ab"));
        assert!(fits(&["a", "b", "c"], "a1b2c"));
        assert!(fits(&["a", "b", "c"], "abc"));
        assert!(!fits(&["a", "b", "c"], "acb"));
        assert!(!fits(&["ab", "ba"], "aba"));
    }

    #[test]
    fn other_names_are_no_patterns() {
        assert!(key_pattern("onClick").is_none());
        assert!(key_pattern("data-x").is_none());
    }
}
