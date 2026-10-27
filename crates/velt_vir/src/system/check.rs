//! A `declare system function` against the table: which function of it the declaration binds,
//! or why it binds none. One check for veltc (which reports it) and lowering (which follows it).

use velt_sema::hir::{self, Def, DefId, IntTy, TyId, TyKind};

use super::{base_name, overloads, Func, Int, Param, Ret, Struct, TARGETS};

/// Why a declaration binds no function of the table.
#[derive(Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// No target has a function of this name; `close` is the most similar one, if any.
    Unknown { close: Option<&'static str> },
    /// `declare async system function` or generics.
    Form(&'static str),
    /// No overload of the target (or, for a function this target lacks, of a target that has
    /// it) has this signature: per overload, the first difference, and the declarations that
    /// would match.
    Signature {
        reasons: Vec<String>,
        expected: Vec<String>,
        prototypes: Vec<String>,
    },
}

/// The function of `triple`'s table that `f` binds: `Ok(None)` when `triple` has no function of
/// that name but another target has one and `f` matches it (calls are then an error, at the
/// call). A declaration names the function, with an optional `$suffix`, and states its Velt
/// types; the overload whose parameters they fit is the one bound.
pub fn match_decl(
    p: &hir::Program,
    f: &hir::ExternFnDef,
    triple: &str,
) -> Result<Option<&'static Func>, Mismatch> {
    if f.is_async {
        return Err(Mismatch::Form(
            "a system function is synchronous: `declare system function`, not `async`",
        ));
    }
    let here: Vec<&Func> = overloads(triple, &f.symbol).collect();
    let (candidates, local) = match here.is_empty() {
        false => (here, true),
        true => {
            let other = TARGETS
                .iter()
                .map(|t| overloads(t, &f.symbol).collect::<Vec<_>>())
                .find(|o| !o.is_empty());
            match other {
                Some(o) => (o, false),
                None => {
                    let names = super::all_names();
                    let close = closest(base_name(&f.symbol), &names);
                    return Err(Mismatch::Unknown { close });
                }
            }
        }
    };
    let mut reasons = vec![];
    for c in &candidates {
        match fits(p, f, c) {
            Ok(()) => return Ok(local.then_some(*c)),
            Err(why) => reasons.push(why),
        }
    }
    reasons.dedup();
    Err(Mismatch::Signature {
        reasons,
        expected: candidates.iter().map(|c| velt_decl(c)).collect(),
        prototypes: candidates.iter().map(|c| c.prototype()).collect(),
    })
}

/// Does a declaration with `declared` parameters give `c`'s buffer as a range, `buf: u8[],
/// start: usize, end: usize` (the bytes `start..end` of the array)? Only for functions with one
/// buffer.
pub fn ranged(c: &Func, declared: usize) -> bool {
    let params = c.params.iter().filter(|p| p.declared()).count();
    let bufs = c.params.iter().filter(|p| matches!(p, Param::Buf { .. })).count();
    bufs == 1 && declared == params + 2
}

/// The declarations that bind `name` on `triple` (for documentation and messages).
pub fn expected_decls(triple: &str, name: &str) -> Vec<String> {
    overloads(triple, name).map(velt_decl).collect()
}

/// A Velt declaration of `c`: `declare system function pread(fd: i32, buf: u8[], offset: i64):
/// isize;`. A struct parameter is shown with a type named after the C struct.
fn velt_decl(c: &Func) -> String {
    let ps: Vec<String> = c
        .params
        .iter()
        .filter(|p| p.declared())
        .map(|p| match p {
            Param::Int { name, ty, .. } => format!("{name}: {}", ty.velt()),
            Param::Buf { name, written, .. } => {
                format!("{name}: {}u8[]", if *written { "" } else { "readonly " })
            }
            Param::CStr { name } => format!("{name}: readonly u8[]"),
            Param::WStr { name } => format!("{name}: readonly u16[]"),
            Param::Struct { name, layout } => format!("{name}: {}", struct_name(layout)),
            Param::Null { .. } => unreachable!("ICE: left out"),
        })
        .collect();
    let ret = match c.ret {
        Ret::Int { ty, .. } => ty.velt(),
        Ret::Errno => "i32",
    };
    format!(
        "declare system function {}({}): {ret};",
        c.name,
        ps.join(", ")
    )
}

/// `struct flock` → `Flock`, `OVERLAPPED` → `Overlapped`: the name the docs give the Velt struct.
fn struct_name(s: &Struct) -> String {
    let c = s.c.trim_start_matches("struct ");
    let mut out = String::new();
    for part in c.split('_') {
        let lower = part.to_ascii_lowercase();
        let mut cs = lower.chars();
        if let Some(first) = cs.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(cs);
        }
    }
    out
}

/// A Velt type as a system function sees it.
enum Shape {
    Int(Int),
    /// `u8[]` / `u16[]` (whether written `readonly` comes from the declaration).
    Bytes,
    Words,
    Struct(DefId),
    Other(String),
}

fn shape(p: &hir::Program, t: TyId) -> Shape {
    match p.types.kind(t) {
        TyKind::Int(i) => Shape::Int(match i {
            IntTy::I16 => Int::I16,
            IntTy::U16 => Int::U16,
            IntTy::I32 => Int::I32,
            IntTy::U32 => Int::U32,
            IntTy::I64 => Int::I64,
            IntTy::U64 => Int::U64,
            IntTy::ISize => Int::ISize,
            IntTy::USize => Int::USize,
            IntTy::I8 => return Shape::Other("i8".into()),
            IntTy::U8 => return Shape::Other("u8".into()),
        }),
        TyKind::Array(e) => match p.types.kind(*e) {
            TyKind::Int(IntTy::U8) => Shape::Bytes,
            TyKind::Int(IntTy::U16) => Shape::Words,
            _ => Shape::Other("an array of anything but `u8` or `u16`".into()),
        },
        TyKind::Adt(d, args) if args.is_empty() => match p.def(*d) {
            Def::Adt(a) if a.declared_struct => Shape::Struct(*d),
            Def::Adt(a) => Shape::Other(format!("`{}`", a.name)),
            _ => Shape::Other("this type".into()),
        },
        TyKind::Str => Shape::Other("`string`".into()),
        TyKind::Float(_) => Shape::Other("a floating-point number".into()),
        TyKind::Bool => Shape::Other("`bool`".into()),
        TyKind::FnPtr { .. } | TyKind::Closure(_) => Shape::Other("a function".into()),
        _ => Shape::Other("this type".into()),
    }
}

fn shown(s: &Shape) -> String {
    match s {
        Shape::Int(i) => format!("`{}`", i.velt()),
        Shape::Bytes => "`u8[]`".into(),
        Shape::Words => "`u16[]`".into(),
        Shape::Struct(_) => "a struct".into(),
        Shape::Other(o) => o.clone(),
    }
}

/// Does `f` declare `c`? The first difference otherwise.
fn fits(p: &hir::Program, f: &hir::ExternFnDef, c: &Func) -> Result<(), String> {
    let params: Vec<&Param> = c.params.iter().filter(|p| p.declared()).collect();
    let names: Vec<&str> = params.iter().map(|p| param_name(p)).collect();
    let ranged = ranged(c, f.params.len());
    if params.len() != f.params.len() && !ranged {
        return Err(format!(
            "`{}` takes {} parameter{} ({}), not {}",
            c.name,
            params.len(),
            if params.len() == 1 { "" } else { "s" },
            names.join(", "),
            f.params.len()
        ));
    }
    let readonly = f.system.as_deref().unwrap_or(&[]);
    let mut ti = 0;
    for &want in &params {
        let ro = readonly.get(ti).copied().unwrap_or(false);
        param_fits(p, c, want, f.params[ti], ro)?;
        ti += 1;
        if ranged && matches!(want, Param::Buf { .. }) {
            for _ in 0..2 {
                if !matches!(shape(p, f.params[ti]), Shape::Int(Int::USize)) {
                    return Err(format!(
                        "a range of the buffer `{}` is two `usize` parameters after it, `start` \
                         and `end`",
                        param_name(want)
                    ));
                }
                ti += 1;
            }
        }
    }
    let want_ret = match c.ret {
        Ret::Int { ty, .. } => ty,
        Ret::Errno => Int::I32,
    };
    match shape(p, f.ret) {
        Shape::Int(i) if i == want_ret => Ok(()),
        _ if matches!(p.types.kind(f.ret), TyKind::Unit) => Err(format!(
            "`{}` returns `{}`: declare the result",
            c.name,
            want_ret.velt()
        )),
        got => Err(format!(
            "`{}` returns `{}`, not {}",
            c.name,
            want_ret.velt(),
            shown(&got)
        )),
    }
}

fn param_name(p: &Param) -> &'static str {
    match *p {
        Param::Int { name, .. }
        | Param::Buf { name, .. }
        | Param::CStr { name }
        | Param::WStr { name }
        | Param::Struct { name, .. }
        | Param::Null { name, .. } => name,
    }
}

fn param_fits(p: &hir::Program, c: &Func, want: &Param, t: TyId, ro: bool) -> Result<(), String> {
    let got = shape(p, t);
    let name = param_name(want);
    let fun = c.name;
    let no_pointer = "(system functions take no pointers: the array gives the address and the \
                      length)";
    match (want, &got) {
        (Param::Int { ty, c: ct, .. }, Shape::Int(i)) if i == ty => Ok(()),
        (Param::Int { ty, c: ct, .. }, _) => Err(format!(
            "`{name}` is a C `{ct}`: declare it `{}`, not {}",
            ty.velt(),
            shown(&got)
        )),
        (Param::Buf { written, .. }, Shape::Bytes) => match (*written, ro) {
            (true, true) => Err(format!(
                "`{fun}` writes into `{name}`: declare it `u8[]`, not `readonly u8[]`"
            )),
            (false, false) => Err(format!(
                "`{fun}` only reads `{name}`: declare it `readonly u8[]`"
            )),
            _ => Ok(()),
        },
        (Param::Buf { written, .. }, _) => Err(format!(
            "`{name}` is a buffer: declare it `{}u8[]`, not {} {no_pointer}",
            if *written { "" } else { "readonly " },
            shown(&got)
        )),
        (Param::CStr { .. }, Shape::Bytes) if ro => Ok(()),
        (Param::CStr { .. }, _) => Err(format!(
            "`{name}` is a C string: declare it `readonly u8[]` (its last byte must be 0), not {}{}",
            if matches!(got, Shape::Bytes) { "`u8[]`".to_string() } else { shown(&got) },
            if matches!(got, Shape::Int(_)) { format!(" {no_pointer}") } else { String::new() }
        )),
        (Param::WStr { .. }, Shape::Words) if ro => Ok(()),
        (Param::WStr { .. }, _) => Err(format!(
            "`{name}` is a UTF-16 string: declare it `readonly u16[]` (its last element must \
             be 0), not {}",
            if matches!(got, Shape::Words) { "`u16[]`".to_string() } else { shown(&got) }
        )),
        (Param::Struct { layout, .. }, Shape::Struct(d)) => struct_fits(p, *d, layout, name),
        (Param::Struct { layout, .. }, _) => Err(format!(
            "`{name}` is a `{} *`: declare it as a `struct` whose fields are fields of `{}`, not \
             {}",
            layout.c,
            layout.c,
            shown(&got)
        )),
        (Param::Null { .. }, _) => unreachable!("ICE: left out"),
    }
}

/// The fields of the Velt struct `d` must each be a field of `layout`, of the field's Velt type.
fn struct_fits(p: &hir::Program, d: DefId, layout: &Struct, name: &str) -> Result<(), String> {
    let Def::Adt(a) = p.def(d) else {
        return Err(format!("`{name}` is not a struct"));
    };
    let all = || {
        layout
            .fields
            .iter()
            .map(|f| format!("{}: {}", f.name, f.ty.velt()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if a.fields.is_empty() {
        return Err(format!(
            "`{}` has no fields; `{}` has {}",
            a.name,
            layout.c,
            all()
        ));
    }
    for fd in &a.fields {
        let Some(cf) = layout.fields.iter().find(|f| f.name == fd.name) else {
            return Err(format!(
                "`{}.{}` is not a field of `{}`, which has {}",
                a.name,
                fd.name,
                layout.c,
                all()
            ));
        };
        if fd.optional {
            return Err(format!(
                "`{}.{}` is optional: a C struct's fields are always there",
                a.name, fd.name
            ));
        }
        match shape(p, fd.ty) {
            Shape::Int(i) if i == cf.ty => {}
            got => {
                return Err(format!(
                    "`{}.{}` must be `{}` (`{}`'s field), not {}",
                    a.name,
                    fd.name,
                    cf.ty.velt(),
                    layout.c,
                    shown(&got)
                ))
            }
        }
    }
    Ok(())
}

/// The most similar name, by edit distance (at most a third of the name's length).
fn closest(name: &str, names: &[&'static str]) -> Option<&'static str> {
    let limit = name.len().max(3) / 3;
    names
        .iter()
        .map(|n| (distance(&name.to_lowercase(), &n.to_lowercase()), *n))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, n)| n)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}
