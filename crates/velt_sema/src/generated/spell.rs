//! Types as generated code writes them: Velt type syntax in which every declared type is named
//! `__qualified<"<module path>", "<name>", …arguments>` (resolved by `Ctx::resolve_qualified`
//! in generated files only), so generated code needs no import and can name types that are not
//! exported. The spelling is stable from one check to the next, unlike type ids.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, FloatTy, IntTy, LitValue, Nothing, TyId, TyKind};

/// The placeholder [`spell_template`] writes for type parameter `i`.
/// How the members of a generic union in `__union<[members], …>` name its type parameter `i`
/// (`__T0`).
pub const UNION_PARAM: &str = "__T";

pub fn param_placeholder(i: u32) -> String {
    format!("@@T{i}@@")
}

/// [`spell`] of a type of a generic signature, its type parameters written as
/// [`param_placeholder`]s (the driver substitutes each instance's type arguments).
pub(crate) fn spell_template(cx: &mut Ctx, t: TyId) -> Option<String> {
    spell_depth(cx, t, 0, true)
}

/// `__qualified<…>` of the declaration whose qualified name is `qual_name` (the root module's
/// declarations have no module prefix); `None` when it is not a module-level declaration.
fn qualified(cx: &Ctx, qual_name: &str, args: &[String]) -> Option<String> {
    let (m, name) = match qual_name.rsplit_once("::") {
        Some((module, name)) => (cx.modules.iter().position(|m| m.path == module)?, name),
        None => (cx.root, qual_name),
    };
    cx.scopes[m].items.get(name)?;
    let module = &cx.modules[m].path;
    let mut s = format!("{}<{:?}, {:?}", super::QUALIFIED, module, name);
    for a in args {
        s.push_str(", ");
        s.push_str(a);
    }
    s.push('>');
    Some(s)
}

pub fn int_name(it: IntTy) -> &'static str {
    match it {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::ISize => "isize",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
        IntTy::USize => "usize",
    }
}

pub fn float_name(ft: FloatTy) -> &'static str {
    match ft {
        FloatTy::F32 => "f32",
        FloatTy::F64 => "f64",
    }
}

fn all(cx: &mut Ctx, ts: &[TyId], depth: u32, params: bool) -> Option<Vec<String>> {
    ts.iter()
        .map(|t| spell_depth(cx, *t, depth + 1, params))
        .collect()
}

fn spell_depth(cx: &mut Ctx, t: TyId, depth: u32, params: bool) -> Option<String> {
    if depth > 64 {
        return None;
    }
    let d1 = depth + 1;
    Some(match cx.ty.kind(t).clone() {
        TyKind::Int(it) => int_name(it).into(),
        TyKind::Float(ft) => float_name(ft).into(),
        TyKind::Bool => "boolean".into(),
        TyKind::Str => "string".into(),
        TyKind::Unit => "void".into(),
        TyKind::Never => "never".into(),
        TyKind::Symbol => "symbol".into(),
        TyKind::Simd(s) => s.name().into(),
        TyKind::Literal(l) => match l {
            LitValue::Str(s) => format!("{s:?}"),
            LitValue::Int(_, v) => v.to_string(),
            LitValue::Float(_, bits) => format!("{}", f64::from_bits(bits)),
            LitValue::Bool(b) => b.to_string(),
        },
        TyKind::Array(e) => format!("({})[]", spell_depth(cx, e, d1, params)?),
        TyKind::Tuple(ts) => format!("[{}]", all(cx, &ts, depth, params)?.join(", ")),
        TyKind::Option(x, n) => {
            let nothing = match n {
                Nothing::Null => "null",
                Nothing::Undefined => "undefined",
                Nothing::Both => "null | undefined",
            };
            format!("({}) | {nothing}", spell_depth(cx, x, d1, params)?)
        }
        TyKind::Map(k, v) => {
            let k = spell_depth(cx, k, d1, params)?;
            format!("Map<{k}, {}>", spell_depth(cx, v, d1, params)?)
        }
        TyKind::Promise(x, _) => format!("Promise<{}>", spell_depth(cx, x, d1, params)?),
        TyKind::Shared(x) => format!("shared<{}>", spell_depth(cx, x, d1, params)?),
        TyKind::FnPtr {
            params: ps_in,
            ret,
            rest,
            ..
        } => {
            let ps = all(cx, &ps_in, depth, params)?;
            let n = ps.len();
            let ps: Vec<String> = ps
                .into_iter()
                .enumerate()
                .map(|(i, p)| match rest && i + 1 == n {
                    true => format!("...p{i}: {p}"),
                    false => format!("p{i}: {p}"),
                })
                .collect();
            format!(
                "(({}) => {})",
                ps.join(", "),
                spell_depth(cx, ret, d1, params)?
            )
        }
        TyKind::Adt(d, args) => match &cx.info[d.0 as usize] {
            // An object type a top-level alias declared: by the alias (it may name itself).
            DefInfo::Adt(a)
                if a.kind == AdtKind::Anon && cx.gen.binding.alias_homes.contains_key(&t) =>
            {
                alias_spelling(cx, t)?
            }
            DefInfo::Adt(a) if a.kind == AdtKind::Anon => {
                let fields: Vec<(String, TyId, bool, bool)> = (a.fields.iter())
                    .map(|f| (f.name.clone(), f.declared, f.optional, f.readonly))
                    .collect();
                let mut fs = vec![];
                for (name, declared, optional, readonly) in fields {
                    let ty = cx.subst(declared, &args);
                    // A function with properties: a type literal would be another type; the
                    // alias that declared it names it.
                    if name.starts_with('#')
                        || name.starts_with(crate::property_order::SYMBOL_KEY_PREFIX)
                    {
                        return alias_spelling(cx, t);
                    }
                    let opt = if optional { "?" } else { "" };
                    let ro = if readonly { "readonly " } else { "" };
                    fs.push(format!(
                        "{ro}{name:?}{opt}: {}",
                        spell_depth(cx, ty, d1, params)?
                    ));
                }
                format!("{{ {} }}", fs.join("; "))
            }
            DefInfo::Adt(a) => {
                let q = a.qual_name.clone();
                let a = all(cx, &args, depth, params)?;
                // An object literal type with accessors: by the alias that declared it.
                match qualified(cx, &q, &a) {
                    Some(s) => s,
                    None => alias_spelling(cx, t)?,
                }
            }
            // A union a top-level alias declared: by the alias (it may name itself).
            DefInfo::Enum(e) if e.is_union && cx.gen.binding.alias_homes.contains_key(&t) => {
                alias_spelling(cx, t)?
            }
            // A generic union at its type arguments (`T | Mod<T>` at `T = unknown`, which
            // written out would be `unknown`): `__union<[members], …arguments>`.
            DefInfo::Enum(e) if e.is_union && !args.is_empty() => {
                let payloads: Option<Vec<TyId>> = (e.variants.iter())
                    .map(|v| v.payload.first().copied())
                    .collect();
                let mut arms = vec![];
                for p in payloads? {
                    let s = spell_depth(cx, p, d1, true)?;
                    let mut s = s;
                    for i in 0..args.len() {
                        s = s.replace(&param_placeholder(i as u32), &format!("{UNION_PARAM}{i}"));
                    }
                    arms.push(s);
                }
                let a = all(cx, &args, depth, params)?;
                format!("{}<[{}], {}>", super::UNION, arms.join(", "), a.join(", "))
            }
            DefInfo::Enum(e) if e.is_union => {
                let payloads: Option<Vec<TyId>> = e
                    .variants
                    .iter()
                    .map(|v| v.payload.first().copied())
                    .collect();
                let mut arms = vec![];
                for p in payloads? {
                    let p = cx.subst(p, &args);
                    arms.push(spell_depth(cx, p, d1, params)?);
                }
                format!("({})", arms.join(" | "))
            }
            DefInfo::Enum(e) => {
                let q = e.qual_name.clone();
                {
                    let a = all(cx, &args, depth, params)?;
                    qualified(cx, &q, &a)?
                }
            }
            _ => return None,
        },
        TyKind::Dyn(d, args) => match &cx.info[d.0 as usize] {
            DefInfo::Iface(i) => {
                let q = i.qual_name.clone();
                {
                    let a = all(cx, &args, depth, params)?;
                    qualified(cx, &q, &a)?
                }
            }
            _ => return None,
        },
        TyKind::Error | TyKind::Result(..) | TyKind::Closure(_) | TyKind::Op(..) => return None,
        TyKind::Param(i) if params => param_placeholder(i),
        TyKind::Param(_) => return None,
    })
}

/// `__qualified<"<module>", "<alias>">` for an object type a top-level alias declared.
fn alias_spelling(cx: &mut Ctx, t: TyId) -> Option<String> {
    let (m, name, non_null) = cx.gen.binding.alias_homes.get(&t)?.clone();
    let path = cx.modules[m].path.clone();
    let q = qualified(cx, &format!("{path}::{name}"), &[])?;
    Some(match non_null {
        true => format!("NonNullable<{q}>"),
        false => q,
    })
}
