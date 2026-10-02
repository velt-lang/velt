//! Build the final `hir::Def`s for type definitions, constants and externs (function bodies are
//! already built by the body pass), and assemble the `hir::Program`.

use std::collections::HashMap;

use crate::collect::{self_type, DISPOSE};
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{
    self, AdtDef, Def, DefId, EnumDef, ExternFnDef, FieldDef, GlobalDef, InterfaceDef,
    InterfaceMethodDef, VariantDef,
};

pub(crate) fn build_defs(cx: &mut Ctx) {
    let mut memo: HashMap<DefId, Vec<Option<hir::Expr>>> = HashMap::new();
    let assigned = crate::assigned_fields::assigned_objects(cx);
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        if cx.defs[i].is_some() {
            let mut throws = cx.try_fn(d).and_then(|f| f.throws);
            // A synchronous forwarder to an async method: its group's errors reject the promise
            // it returns, they are not thrown by the call (throws/groups.rs `Group::promise`).
            if cx.try_fn(d).is_some_and(|f| !f.is_async) && cx.throw_groups().in_promise_group(d) {
                throws = None;
            }
            if let Some(Def::Fn(f)) = &mut cx.defs[i] {
                f.throws = throws;
            }
            continue;
        }
        let def = match &cx.info[i] {
            DefInfo::Fn(f) => Some(Def::ExternFn(ExternFnDef {
                name: f.name.clone(),
                symbol: f.name.clone(),
                params: f.params.iter().map(|p| p.ty).collect(),
                ret: f.ret,
                is_async: f.is_async,
                span: f.span,
            })),
            DefInfo::Adt(_) => Some(Def::Adt(adt_def(cx, d, &mut memo, assigned.contains(&d)))),
            DefInfo::Enum(_) => Some(Def::Enum(enum_def(cx, d))),
            DefInfo::Iface(i) => Some(Def::Interface(InterfaceDef {
                name: i.qual_name.clone(),
                generics: i.generics.len() as u32,
                fields: i
                    .fields
                    .iter()
                    .map(|f| FieldDef {
                        name: f.name.clone(),
                        ty: f.ty,
                        default: None,
                    })
                    .collect(),
                methods: i
                    .methods
                    .iter()
                    .map(|m| InterfaceMethodDef {
                        name: m.name.clone(),
                        default: m.default,
                    })
                    .chain(i.fields.iter().map(|f| InterfaceMethodDef {
                        name: format!("<{}>", f.name),
                        default: None,
                    }))
                    .collect(),
                span: i.span,
            })),
            DefInfo::Global(g) => g.init.clone().map(|init| {
                Def::Global(GlobalDef {
                    name: g.qual_name.clone(),
                    ty: g.ty,
                    init,
                    span: g.span,
                })
            }),
        };
        cx.defs[i] = def;
    }
}

/// Field defaults of a class incl. inherited ones (base defaults with the base's type args).
fn field_defaults(
    cx: &mut Ctx,
    d: DefId,
    memo: &mut HashMap<DefId, Vec<Option<hir::Expr>>>,
) -> Vec<Option<hir::Expr>> {
    if let Some(v) = memo.get(&d) {
        return v.clone();
    }
    let a = cx.adt(d).expect("ICE: adt");
    let (start, base, own): (usize, Option<hir::TyId>, Vec<Option<hir::Expr>>) = (
        a.own_fields_start,
        a.base,
        a.fields[a.own_fields_start..]
            .iter()
            .map(|f| f.default.clone())
            .collect(),
    );
    let mut out = vec![];
    if let Some((bd, bargs)) = base.and_then(|b| cx.class_of(b)) {
        for mut e in field_defaults(cx, bd, memo).into_iter().take(start) {
            if let Some(e) = &mut e {
                crate::visit::map_expr_types(e, &mut |t| cx.ty.subst(t, &bargs));
            }
            out.push(e);
        }
    }
    out.resize(start, None);
    out.extend(own);
    memo.insert(d, out.clone());
    out
}

fn adt_def(
    cx: &mut Ctx,
    d: DefId,
    memo: &mut HashMap<DefId, Vec<Option<hir::Expr>>>,
    assigned: bool,
) -> AdtDef {
    let defaults = field_defaults(cx, d, memo);
    let a = cx.adt(d).expect("ICE: adt");
    let n = a.generics.len();
    let private_fields = a.fields.iter().any(|f| f.private_to.is_some());
    let fields = a
        .fields
        .iter()
        .zip(defaults)
        .map(|(f, default)| FieldDef {
            name: f.name.clone(),
            ty: f.ty,
            default,
        })
        .collect();
    let (name, kind, base, ctor, vtable, span) = (
        a.qual_name.clone(),
        a.kind,
        a.base,
        a.ctor,
        a.vtable.clone(),
        a.span,
    );
    let st = self_type(cx, d, n);
    let self_args: Vec<hir::TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let dispose = match crate::collect::lookup_method(cx, d, &self_args, DISPOSE) {
        Some(f @ crate::collect::Found::Class { .. }) if !f.is_static() => Some(f.def()),
        _ => None,
    };
    AdtDef {
        name,
        kind,
        generics: n as u32,
        fields,
        is_copy: cx.is_copy(st),
        private_fields,
        assigned,
        base,
        ctor,
        dispose,
        vtable,
        span,
    }
}

fn enum_def(cx: &mut Ctx, d: DefId) -> EnumDef {
    let e = cx.enum_info(d).expect("ICE: enum");
    let n = e.generics.len();
    let (name, span) = (e.qual_name.clone(), e.span);
    let e_union = e.is_union;
    let variants = e
        .variants
        .iter()
        .map(|v| VariantDef {
            name: v.name.clone(),
            payload: v.payload.clone(),
            discriminant: v.discriminant,
            str_value: v.str_value.clone(),
        })
        .collect();
    let st = self_type(cx, d, n);
    EnumDef {
        name,
        generics: n as u32,
        variants,
        is_copy: cx.is_copy(st),
        is_union: e_union,
        span,
    }
}
