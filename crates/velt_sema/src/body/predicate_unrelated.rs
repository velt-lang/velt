//! Type predicates whose object type no member of the tested union can be (docs/reference/types.md
//! "Type predicates"; sigx's `isComputed(children)` on a `JSXChild`): TypeScript narrows to the
//! intersection of the member and the predicate's type. A member is ruled out only when the
//! predicate's type has a required member that no value of the member's type has: a string, an
//! array or a function has `length` (and its prototype's methods, and `Object.prototype`'s), so
//! `v is { length: number }` holds for them and narrows nothing here, while `{ value: T }` rules
//! them out. Where the call is true, the local holds only the members not ruled out (`string` of
//! `string | number` for `{ length: number }`), or none of them (that code does not run); the
//! run-time check makes a lying predicate a `TypeError`.

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{AdtKind, LitValue, LocalId, TyId, TyKind};

/// The members of `Object.prototype`, which every value but `null` and `undefined` has.
const OBJECT_PROTO: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__proto__",
];

const STRING_PROTO: &[&str] = &[
    "length",
    "at",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "includes",
    "indexOf",
    "isWellFormed",
    "lastIndexOf",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "split",
    "startsWith",
    "substr",
    "substring",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toUpperCase",
    "toWellFormed",
    "trim",
    "trimEnd",
    "trimStart",
];

const ARRAY_PROTO: &[&str] = &[
    "length",
    "at",
    "concat",
    "copyWithin",
    "entries",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "pop",
    "push",
    "reduce",
    "reduceRight",
    "reverse",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "unshift",
    "values",
    "with",
];

const FUNCTION_PROTO: &[&str] = &["length", "name", "apply", "bind", "call"];

const NUMBER_PROTO: &[&str] = &["toExponential", "toFixed", "toPrecision"];

const SYMBOL_PROTO: &[&str] = &["description"];

/// Which values of a member type have a given property, as far as ruling the member out goes.
enum Has<'a> {
    /// Values with these prototype members (and `Object.prototype`'s); `index`: an array-index
    /// key (`"0"`) too.
    Proto(&'a [&'a str], bool),
    /// An object type with exactly these fields (and `Object.prototype`'s members).
    Fields(Vec<String>),
}

impl Has<'_> {
    fn has(&self, name: &str) -> bool {
        match self {
            Has::Proto(names, index) => {
                names.contains(&name)
                    || OBJECT_PROTO.contains(&name)
                    || (*index && !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
            }
            Has::Fields(fields) => fields.iter().any(|f| f == name) || OBJECT_PROTO.contains(&name),
        }
    }
}

impl FnCx<'_, '_> {
    /// Facts of a predicate `v is target` on local `l` (of type `u`, with members `parts`)
    /// where `target` is an object type that is no member of `u` (module docs): when `true`,
    /// the local holds only the members that have every required member of `target`, when
    /// `false` it is unchanged. `None` (no narrowing) when no member is ruled out, or when a
    /// member's properties are not known: an interface value or a class instance (a subclass
    /// may implement `target`).
    pub(super) fn unrelated_predicate_facts(
        &mut self,
        l: LocalId,
        nullable: bool,
        u: TyId,
        target: TyId,
        parts: &[TyId],
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        self.cx.union_def(u)?;
        let required = self.required_members(target)?;
        if required.is_empty() {
            return None;
        }
        let mut kept = vec![];
        for (i, &m) in parts.iter().enumerate() {
            let has = self.member_has(m)?;
            if required.iter().all(|n| has.has(n)) {
                kept.push(i as u32);
            }
        }
        if kept.len() == parts.len() {
            return None;
        }
        let mut t = vec![Fact::Members(l, kept)];
        if nullable {
            t.push(Fact::NonNull(l));
        }
        Some((t, vec![]))
    }

    /// The names of the required (not optional) fields of the object type `t` (an anonymous
    /// object type, or an interface with fields only, `Ctx::field_only_of`); `None` for any
    /// other type.
    fn required_members(&self, t: TyId) -> Option<Vec<String>> {
        let TyKind::Adt(d, _) = self.cx.ty.kind(t) else {
            return None;
        };
        let a = self.cx.adt(*d)?;
        if a.kind != AdtKind::Anon && !self.cx.field_only_of.contains_key(d) {
            return None;
        }
        Some(
            a.fields
                .iter()
                .filter(|f| !f.optional)
                .map(|f| f.name.clone())
                .collect(),
        )
    }

    /// The properties the values of member type `m` have (`None`: not known, so `m` is never
    /// ruled out: an interface value, a class instance, `any`, a type parameter).
    fn member_has(&self, m: TyId) -> Option<Has<'static>> {
        Some(match self.cx.ty.kind(m) {
            TyKind::Str | TyKind::Literal(LitValue::Str(_)) => Has::Proto(STRING_PROTO, true),
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Literal(LitValue::Int(..) | LitValue::Float(..)) => {
                Has::Proto(NUMBER_PROTO, false)
            }
            TyKind::Bool | TyKind::Literal(LitValue::Bool(_)) => Has::Proto(&[], false),
            TyKind::Symbol => Has::Proto(SYMBOL_PROTO, false),
            TyKind::Array(_) | TyKind::Tuple(_) => Has::Proto(ARRAY_PROTO, true),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => Has::Proto(FUNCTION_PROTO, false),
            TyKind::Adt(d, _) => {
                let a = self.cx.adt(*d)?;
                if !matches!(a.kind, AdtKind::Anon | AdtKind::Struct) {
                    return None;
                }
                Has::Fields(a.fields.iter().map(|f| f.name.clone()).collect())
            }
            _ => return None,
        })
    }
}
