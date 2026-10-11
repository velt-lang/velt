//! Invariant 10: debug variables refer to existing debug types, aggregates and fields.

use crate::vir::*;

pub(super) fn check_debug(p: &Program, errs: &mut Vec<String>) {
    let n = p.debug_types.len();
    let ty_ok = |t: DebugTyId| (t.0 as usize) < n;
    for (i, t) in p.debug_types.iter().enumerate() {
        let mut bad = |what: String| errs.push(format!("debug type #{i} `{}`: {what}", t.name));
        let fields_ok = |agg: AggId, fields: &[DebugField]| {
            let Some(a) = p.aggs.get(agg.0 as usize) else {
                return Err(format!("unknown agg#{}", agg.0));
            };
            match fields
                .iter()
                .find(|f| f.index as usize >= a.fields.len() || !ty_ok(f.ty))
            {
                Some(f) => Err(format!("bad field `{}` of agg#{}", f.name, agg.0)),
                None => Ok(()),
            }
        };
        let checked = match &t.kind {
            DebugKind::Scalar(_)
            | DebugKind::Str
            | DebugKind::Enum { .. }
            | DebugKind::Opaque(_) => Ok(()),
            DebugKind::Array { elem: inner } | DebugKind::Option { inner, .. } => {
                match ty_ok(*inner) {
                    true => Ok(()),
                    false => Err(format!("unknown debug type #{}", inner.0)),
                }
            }
            DebugKind::Shared { boxed, inner } => match ty_ok(*inner) {
                true => fields_ok(*boxed, &[]),
                false => Err(format!("unknown debug type #{}", inner.0)),
            },
            DebugKind::Struct { agg, fields } => fields_ok(*agg, fields),
            DebugKind::Class { obj, fields } => fields_ok(*obj, fields),
            DebugKind::Tagged { agg, variants } => fields_ok(*agg, &[]).and_then(|()| {
                variants
                    .iter()
                    .try_for_each(|v| fields_ok(v.view, &v.fields))
            }),
        };
        if let Err(e) = checked {
            bad(e);
        }
    }
    for f in &p.funcs {
        for (li, l) in f.locals.iter().enumerate() {
            let Some(d) = &l.debug else { continue };
            if !ty_ok(d.ty) {
                errs.push(format!(
                    "{}: local _{li} has unknown debug type #{}",
                    f.symbol, d.ty.0
                ));
            }
            if d.decl.file as usize >= p.files.len() {
                errs.push(format!(
                    "{}: local _{li} is declared in unknown file {}",
                    f.symbol, d.decl.file
                ));
            }
            if d.by_ref && l.ty != Ty::Ptr {
                errs.push(format!(
                    "{}: local _{li} holds its variable by reference but is not a Ptr",
                    f.symbol
                ));
            }
        }
    }
}
