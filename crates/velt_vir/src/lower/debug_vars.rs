//! Debug descriptions of source variables (vir/debug.rs, `LowerOptions::debug_info`): the debug
//! type of each concrete HIR type, in terms of the layouts lowering builds for it (layout.rs),
//! and the `LocalDebug` of each VIR local that holds a source variable.

use std::collections::HashMap;

use velt_sema::hir::{self, AdtKind, FloatTy, FnDef, TyId, TyKind};

use super::{ice, Cx, FnLower};
use crate::vir::{DebugField, DebugKind, DebugTy, DebugTyId, DebugVariant, LocalDebug, Ty};

/// The debug types of a program being lowered (`Program::debug_types`).
#[derive(Default)]
pub(super) struct DebugTypes {
    pub(super) types: Vec<DebugTy>,
    memo: HashMap<TyId, DebugTyId>,
}

impl Cx<'_> {
    /// The debug type of the concrete type `t`, described on first use.
    pub(super) fn debug_ty(&mut self, t: TyId) -> DebugTyId {
        let d = self.debug_types();
        if let Some(&id) = d.memo.get(&t) {
            return id;
        }
        // Registered before its parts, so a type reached again through itself (a class with a
        // field of its own type) refers to this entry.
        let id = DebugTyId(d.types.len() as u32);
        d.types.push(DebugTy {
            name: String::new(),
            kind: DebugKind::Opaque(Ty::Unit),
        });
        d.memo.insert(t, id);
        let name = self.debug_name(t);
        let kind = self.debug_kind(t);
        self.debug_types().types[id.0 as usize] = DebugTy { name, kind };
        id
    }

    fn debug_types(&mut self) -> &mut DebugTypes {
        self.debug
            .as_mut()
            .unwrap_or_else(|| ice("debug type requested without debug info"))
    }

    fn debug_kind(&mut self, t: TyId) -> DebugKind {
        match self.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => DebugKind::Scalar(self.ty(t)),
            TyKind::Str => DebugKind::Str,
            _ if self.boxed(t) => DebugKind::Opaque(Ty::Ptr),
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Adt(a) => {
                    let names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
                    let tys = self.adt_field_tys(t);
                    let fields = self.debug_fields(t, names, tys);
                    if a.kind == AdtKind::Class {
                        DebugKind::Class {
                            obj: self.obj_agg(t),
                            fields,
                        }
                    } else {
                        DebugKind::Struct {
                            agg: self.value_agg(t),
                            fields,
                        }
                    }
                }
                hir::Def::Enum(e) if self.is_c_like_enum(d) => DebugKind::Enum {
                    members: e
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| match v.str_value {
                            Some(_) => (v.name.clone(), i as i64),
                            None => (v.name.clone(), v.discriminant),
                        })
                        .collect(),
                },
                hir::Def::Enum(e) if e.is_union => {
                    // A union's variants are its members, named as written.
                    let n = e.variants.len() as u32;
                    let names = (0..n)
                        .map(|v| {
                            let member = self.variant_tys(t, v)[0];
                            self.debug_name(member)
                        })
                        .collect();
                    self.debug_tagged(t, names)
                }
                hir::Def::Enum(e) => {
                    let names = e.variants.iter().map(|v| v.name.clone()).collect();
                    self.debug_tagged(t, names)
                }
                _ => DebugKind::Opaque(self.ty(t)),
            },
            TyKind::Result(..) => self.debug_tagged(t, vec!["Ok".into(), "Err".into()]),
            TyKind::Tuple(es) => {
                let names = (0..es.len()).map(|i| i.to_string()).collect();
                let fields = self.debug_fields(t, names, es);
                DebugKind::Struct {
                    agg: self.value_agg(t),
                    fields,
                }
            }
            TyKind::Option(inner) => DebugKind::Option {
                repr: self.ty(t),
                inner: self.debug_ty(inner),
            },
            TyKind::Array(e) => DebugKind::Array {
                elem: self.debug_ty(e),
            },
            TyKind::Shared(e) => DebugKind::Shared {
                boxed: self.shared_box(e),
                inner: self.debug_ty(e),
            },
            _ => DebugKind::Opaque(self.ty(t)),
        }
    }

    /// The stored fields (not `void` ones) of struct / class / tuple `t`, named `names`.
    fn debug_fields(&mut self, t: TyId, names: Vec<String>, tys: Vec<TyId>) -> Vec<DebugField> {
        let mut fields = vec![];
        for (i, (name, ft)) in names.into_iter().zip(tys).enumerate() {
            if !self.is_unit(ft) {
                fields.push(DebugField {
                    name,
                    index: self.vir_field(t, i as u32),
                    ty: self.debug_ty(ft),
                });
            }
        }
        fields
    }

    /// An enum with payloads, union or result: variant `i`'s payloads follow the tag in its
    /// view (layout.rs `variant_views`). A union's single payload is shown as `value`.
    fn debug_tagged(&mut self, t: TyId, names: Vec<String>) -> DebugKind {
        let agg = self.value_agg(t);
        let union = self.is_union(t);
        let mut variants = vec![];
        for (v, name) in names.into_iter().enumerate() {
            let view = self.view(t, v as u32);
            let mut fields = vec![];
            for (k, p) in self.variant_tys(t, v as u32).into_iter().enumerate() {
                if self.is_unit(p) {
                    continue;
                }
                fields.push(DebugField {
                    name: if union { "value".into() } else { k.to_string() },
                    index: fields.len() as u32 + 1,
                    ty: self.debug_ty(p),
                });
            }
            variants.push(DebugVariant { name, view, fields });
        }
        DebugKind::Tagged { agg, variants }
    }

    /// `t` as written in source: `number`, `Point`, `Box<string>`, `number[]`, `T | null`.
    fn debug_name(&mut self, t: TyId) -> String {
        match self.kind(t) {
            TyKind::Int(i) => super::keys::int_key(i).into(),
            TyKind::Float(FloatTy::F64) => "number".into(),
            TyKind::Float(FloatTy::F32) => "f32".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Str => "string".into(),
            TyKind::Unit => "void".into(),
            TyKind::Never => "never".into(),
            TyKind::Array(e) => {
                let elem = self.debug_name(e);
                let grouped = matches!(self.kind(e), TyKind::Option(_) | TyKind::FnPtr { .. })
                    || self.is_union(e);
                match grouped {
                    true => format!("({elem})[]"),
                    false => format!("{elem}[]"),
                }
            }
            TyKind::Map(k, v) => self.debug_name_args("Map".into(), &[k, v]),
            TyKind::Tuple(es) => format!("[{}]", self.debug_names(&es, ", ")),
            TyKind::Option(x) => format!("{} | null", self.debug_name(x)),
            TyKind::Result(a, b) => self.debug_name_args("Result".into(), &[a, b]),
            TyKind::Promise(x, _) => self.debug_name_args("Promise".into(), &[x]),
            TyKind::Shared(x) => self.debug_name_args("shared".into(), &[x]),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => "function".into(),
            TyKind::Adt(d, args) => match self.hir.def(d) {
                hir::Def::Enum(e) if e.is_union => {
                    let n = e.variants.len() as u32;
                    let members: Vec<TyId> = (0..n).map(|v| self.variant_tys(t, v)[0]).collect();
                    self.debug_names(&members, " | ")
                }
                hir::Def::Adt(a) if a.kind == AdtKind::Anon => {
                    let names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
                    let tys = self.adt_field_tys(t);
                    let fs: Vec<String> = names
                        .into_iter()
                        .zip(tys)
                        .map(|(n, ft)| format!("{n}: {}", self.debug_name(ft)))
                        .collect();
                    format!("{{ {} }}", fs.join("; "))
                }
                _ => {
                    let name = self.type_name(t);
                    self.debug_name_args(name, &args)
                }
            },
            TyKind::Dyn(_, args) => {
                let name = self.type_name(t);
                self.debug_name_args(name, &args)
            }
            _ => self.type_key(t),
        }
    }

    fn debug_names(&mut self, ts: &[TyId], sep: &str) -> String {
        let names: Vec<String> = ts.iter().map(|&a| self.debug_name(a)).collect();
        names.join(sep)
    }

    /// `name<args>`, or `name` without type arguments.
    fn debug_name_args(&mut self, name: String, args: &[TyId]) -> String {
        match args {
            [] => name,
            _ => format!("{name}<{}>", self.debug_names(args, ", ")),
        }
    }
}

impl FnLower<'_, '_> {
    /// Describe the locals that hold `f`'s source variables (with `LowerOptions::debug_info`):
    /// params and named locals, as they are bound once the prologue is done. Locals sema made
    /// up (`#arg0`, `__index`) stay undescribed.
    pub(super) fn describe_locals(&mut self, f: &FnDef, params: usize) {
        if self.cx.debug.is_none() {
            return;
        }
        for (i, ld) in f.body.locals.iter().enumerate() {
            let info = &self.info[i];
            let (Some(local), ty, by_ref) = (info.vir, info.ty, info.indirect) else {
                continue;
            };
            if ld.name.starts_with('#') || ld.name.starts_with("__") {
                continue;
            }
            let Some(decl) = self.cx.locs.as_ref().and_then(|m| m.loc(ld.span)) else {
                continue;
            };
            let debug = LocalDebug {
                decl,
                ty: self.cx.debug_ty(ty),
                by_ref,
                param: i < params,
            };
            self.locals[local.0 as usize].debug = Some(debug);
        }
    }
}
