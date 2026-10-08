//! Whether `JSON.parse` can tell the members of a union apart (`union_decode_problem`): by
//! the kind of JSON value, literal members by value, several object members by a discriminant
//! field or by a required key only one of them has.

use crate::ctx::Ctx;
use crate::hir::{LitValue, TyId, TyKind};

/// How a union member is recognized in a JSON document (the decoder's view; velt_vir
/// lower/json/union.rs classifies members the same way).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Str,
    /// String literal types and string enums: matched by value.
    StrLits,
    Num,
    NumLits,
    Bool,
    BoolLits,
    Array,
    /// Structs, classes and object literal types.
    Object,
    /// `Map<string, V>` / `Record<K, V>`: any object.
    Dict,
    /// `JsonValue`: any value.
    Any,
}

fn shape(cx: &Ctx, m: TyId) -> Shape {
    match cx.ty.kind(m) {
        TyKind::Str => Shape::Str,
        TyKind::Int(_) | TyKind::Float(_) => Shape::Num,
        TyKind::Bool => Shape::Bool,
        TyKind::Literal(LitValue::Str(_)) => Shape::StrLits,
        TyKind::Literal(LitValue::Bool(_)) => Shape::BoolLits,
        TyKind::Literal(_) => Shape::NumLits,
        TyKind::Array(_) | TyKind::Tuple(_) => Shape::Array,
        TyKind::Adt(d, _) if cx.is_json_value(*d) => Shape::Any,
        TyKind::Adt(d, _)
            if Some(*d) == cx.prelude_adt("Map") || Some(*d) == cx.prelude_adt("Record") =>
        {
            Shape::Dict
        }
        TyKind::Adt(d, _) => match cx.enum_info(*d) {
            Some(e) if e.variants.iter().any(|v| v.str_value.is_some()) => Shape::StrLits,
            Some(_) => Shape::NumLits,
            None => Shape::Object,
        },
        _ => Shape::Object,
    }
}

/// Why `JSON.parse` cannot decode union `u` (`None`: it can). Members are told apart by the
/// kind of JSON value; literal members by value; several object members by a discriminant (a
/// field with a different literal type in each) or else by a required key only one of them has.
pub(super) fn union_decode_problem(cx: &mut Ctx, u: TyId) -> Option<String> {
    let members = cx.union_members(u)?;
    let shapes: Vec<Shape> = members.iter().map(|m| shape(cx, *m)).collect();
    let of = |want: &[Shape]| -> Vec<TyId> {
        members
            .iter()
            .zip(&shapes)
            .filter(|(_, s)| want.contains(s))
            .map(|(m, _)| *m)
            .collect()
    };
    let both = |cx: &mut Ctx, ms: &[TyId], what: &str| {
        let (a, b) = (cx.display(ms[0]), cx.display(ms[1]));
        format!("`{a}` and `{b}` are both {what}")
    };
    if shapes.contains(&Shape::Any) {
        return Some("a `JsonValue` member takes any JSON value".into());
    }
    if let Some(clash) = literal_clash(cx, &members, &shapes) {
        return Some(clash);
    }
    let nums = of(&[Shape::Num]);
    if nums.len() > 1 {
        return Some(both(cx, &nums, "JSON numbers"));
    }
    let arrays = of(&[Shape::Array]);
    if arrays.len() > 1 {
        return Some(both(cx, &arrays, "JSON arrays"));
    }
    let objects = of(&[Shape::Object, Shape::Dict]);
    if objects.len() > 1 {
        if let Some(dict) = of(&[Shape::Dict]).first() {
            let dn = cx.display(*dict);
            return Some(format!("`{dn}` takes any JSON object"));
        }
        if let Some(clash) = discriminant_clash(cx, &objects) {
            return Some(clash);
        }
        if object_discriminant(cx, &objects).is_none() && required_keys(cx, &objects).is_none() {
            return Some(both(
                cx,
                &objects,
                "objects with no discriminant field and no required field that only one of them has",
            ));
        }
    }
    None
}

/// Field names of object type `t` with whether each is required (not `T | null`).
fn object_fields(cx: &mut Ctx, t: TyId) -> Vec<(String, TyId, bool)> {
    let TyKind::Adt(d, args) = cx.ty.kind(t).clone() else {
        return vec![];
    };
    let fields: Vec<(String, TyId)> = cx
        .adt(d)
        .map(|a| a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect())
        .unwrap_or_default();
    fields
        .into_iter()
        .map(|(n, ft)| {
            let ft = cx.subst(ft, &args);
            let required = !matches!(cx.ty.kind(ft), TyKind::Option(_));
            (n, ft, required)
        })
        .collect()
}

/// A field every object member has with a literal type, the values all different.
fn object_discriminant(cx: &mut Ctx, objects: &[TyId]) -> Option<String> {
    let first = object_fields(cx, objects[0]);
    'names: for (name, _, _) in first {
        let mut seen: Vec<LitValue> = vec![];
        for &m in objects {
            let Some((_, ft)) = cx.field_of(m, &name) else {
                continue 'names;
            };
            let Some(v) = cx.lit_value(ft) else {
                continue 'names;
            };
            if seen.contains(&v) {
                continue 'names;
            }
            seen.push(v);
        }
        return Some(name);
    }
    None
}

/// The discriminant field's values as the decoder compares them: two of them distinct literal
/// types but the same JSON value (`kind: 1` and `kind: 1.0`) cannot be told apart.
fn discriminant_clash(cx: &mut Ctx, objects: &[TyId]) -> Option<String> {
    let name = object_discriminant(cx, objects)?;
    let mut seen: Vec<(TyId, JsonLit)> = vec![];
    for &m in objects {
        let (_, ft) = cx.field_of(m, &name)?;
        let lit = json_lit(&cx.lit_value(ft)?);
        if let Some((other, _)) = seen.iter().find(|(_, l)| *l == lit) {
            let (a, b) = (cx.display(*other), cx.display(m));
            return Some(format!(
                "`{a}.{name}` and `{b}.{name}` are both the JSON {}",
                lit.describe()
            ));
        }
        seen.push((m, lit));
    }
    None
}

/// For each object member, a required field no other member has.
fn required_keys(cx: &mut Ctx, objects: &[TyId]) -> Option<Vec<String>> {
    let fields: Vec<Vec<(String, TyId, bool)>> =
        objects.iter().map(|m| object_fields(cx, *m)).collect();
    let mut keys = vec![];
    for (i, fs) in fields.iter().enumerate() {
        let own = fs.iter().find(|(n, _, required)| {
            *required
                && fields
                    .iter()
                    .enumerate()
                    .all(|(j, other)| j == i || other.iter().all(|(o, _, _)| o != n))
        })?;
        keys.push(own.0.clone());
    }
    Some(keys)
}

/// A JSON string or number a literal member is matched by (numbers as the decoder compares
/// them: as `f64`).
#[derive(PartialEq)]
enum JsonLit {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl JsonLit {
    /// `string "a"`, `number 1`, `true`.
    fn describe(&self) -> String {
        match self {
            JsonLit::Str(s) => format!("string {s:?}"),
            JsonLit::Num(n) => format!("number {n}"),
            JsonLit::Bool(b) => b.to_string(),
        }
    }
}

fn json_lit(v: &LitValue) -> JsonLit {
    match v {
        LitValue::Str(s) => JsonLit::Str(s.clone()),
        LitValue::Int(_, n) => JsonLit::Num(*n as f64),
        LitValue::Float(_, bits) => JsonLit::Num(f64::from_bits(*bits)),
        LitValue::Bool(b) => JsonLit::Bool(*b),
    }
}

/// The values literal and enum member `m` is matched by, each with its name in messages
/// (`"a"`, `1`, `E.A`).
fn member_literals(cx: &Ctx, m: TyId) -> Vec<(String, JsonLit)> {
    // Bool literals are matched by kind (`Shape::BoolLits`), not here.
    let num = |v: &LitValue| match v {
        LitValue::Bool(_) => None,
        v => Some(json_lit(v)),
    };
    match cx.ty.kind(m) {
        TyKind::Literal(v) => num(v).map(|l| (cx.display(m), l)).into_iter().collect(),
        TyKind::Adt(d, _) => match cx.enum_info(*d) {
            Some(e) => e
                .variants
                .iter()
                .map(|v| {
                    let lit = match &v.str_value {
                        Some(s) => JsonLit::Str(s.clone()),
                        None => JsonLit::Num(v.discriminant as f64),
                    };
                    (format!("{}.{}", e.name, v.name), lit)
                })
                .collect(),
            None => vec![],
        },
        _ => vec![],
    }
}

/// Two literal or enum members matched by the same JSON value (`E | "a"` with `E.A = "a"`,
/// `1 | 1.0`): the decoder could not tell which one the document means.
fn literal_clash(cx: &Ctx, members: &[TyId], shapes: &[Shape]) -> Option<String> {
    let mut seen: Vec<(usize, String, JsonLit)> = vec![];
    for (i, (m, s)) in members.iter().zip(shapes).enumerate() {
        if !matches!(s, Shape::StrLits | Shape::NumLits) {
            continue;
        }
        for (name, lit) in member_literals(cx, *m) {
            let earlier = seen.iter().find(|(j, _, l)| *j != i && *l == lit);
            if let Some((_, other, _)) = earlier {
                let value = lit.describe();
                return Some(format!("`{other}` and `{name}` are both the JSON {value}"));
            }
            seen.push((i, name, lit));
        }
    }
    None
}
