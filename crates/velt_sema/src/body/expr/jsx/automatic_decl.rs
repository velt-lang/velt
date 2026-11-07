//! What attributes an element with an automatic runtime may be given, as `tsc` reads them:
//! `JSX.IntrinsicElements[tag]`, or a component's props and `JSX.IntrinsicAttributes`.

use velt_common::Span;
use velt_syntax::ast;

use super::automatic::Automatic;
use crate::body::FnCx;
use crate::hir::{DefId, TyId, TyKind};
use crate::ide::record::Target;

/// A declared attribute.
pub(super) struct Field {
    pub name: String,
    pub ty: TyId,
    pub required: bool,
    /// Where it is declared (for hover and go to definition).
    pub at: Option<(DefId, u32)>,
    /// The type `JSX.IntrinsicAttributes` also gives this name: a value must fit both.
    pub also: Option<TyId>,
}

/// The attributes an element may be given.
pub(super) struct Declared {
    pub fields: Vec<Field>,
    /// The type of the names not listed; `None` makes them errors.
    pub rest: Option<TyId>,
    /// How the type is shown in messages.
    pub shown: String,
}

impl FnCx<'_, '_> {
    /// Props for an element that declares nothing (a fragment): its children are not checked.
    pub(super) fn open_props(&mut self) -> Declared {
        Declared {
            fields: vec![],
            rest: self.any_ty(),
            shown: "{}".to_string(),
        }
    }

    /// The attributes `JSX.IntrinsicElements[tag]` declares, as `tsc` reads it; every declaration
    /// of the interface counts, and an index signature gives any tag. `None` once reported.
    pub(super) fn tag_props(&mut self, a: &Automatic, tag: &str, span: Span) -> Option<Declared> {
        if a.intrinsics.is_empty() {
            self.cx.err(
                "JSX element implicitly has type 'any' because no interface 'JSX.IntrinsicElements' exists.",
                span,
            );
            return None;
        }
        let mut names = vec![];
        let mut by_index = None;
        for t in a.intrinsics.clone() {
            let Some(decl) = self.declared_of(t, String::new()) else {
                continue;
            };
            if let Some(f) = decl.fields.iter().find(|f| f.name == tag) {
                if let Some((d, i)) = f.at {
                    self.cx.rec_ref(span, Target::Field(d, i));
                }
                let shown = format!("JSX.IntrinsicElements[\"{tag}\"]");
                let ty = f.ty;
                return match self.declared_of(ty, shown) {
                    Some(d) => Some(d),
                    None => {
                        self.cx.err(
                            format!("`JSX.IntrinsicElements[\"{tag}\"]` must be an object type of attributes"),
                            span,
                        );
                        None
                    }
                };
            }
            names.extend(decl.fields.into_iter().map(|f| f.name));
            by_index = by_index.or(decl.rest);
        }
        if let Some(t) = by_index {
            let shown = format!("JSX.IntrinsicElements[\"{tag}\"]");
            return self
                .declared_of(t, shown)
                .or_else(|| Some(self.open_props()));
        }
        self.no_property(tag, "JSX.IntrinsicElements", &names, span, None);
        None
    }

    /// The attributes of a component with props type `props`: its fields and those of
    /// `JSX.IntrinsicAttributes` (without `key`, which goes apart). `None` once reported.
    pub(super) fn component_props_decl(
        &mut self,
        a: &Automatic,
        props: TyId,
        slot_names: &[String],
        tag: &str,
        span: Span,
    ) -> Option<Declared> {
        let shown = self.cx.display_in(props, slot_names);
        let shown = match a.attributes.is_empty() {
            true => shown,
            false => format!("IntrinsicAttributes & {shown}"),
        };
        let Some(mut decl) = self.declared_of(props, shown) else {
            let shown = self.cx.display(props);
            self.cx.err(
                format!("the props of component <{tag}> must have an object type, found `{shown}`"),
                span,
            );
            return None;
        };
        for t in a.attributes.clone() {
            let Some(common) = self.declared_of(t, String::new()) else {
                continue;
            };
            for f in common.fields.into_iter().filter(|f| f.name != "key") {
                match decl.fields.iter_mut().find(|o| o.name == f.name) {
                    Some(own) => own.also = Some(f.ty),
                    None => decl.fields.push(f),
                }
            }
        }
        Some(decl)
    }

    /// The `key` field of `JSX.IntrinsicAttributes`.
    pub(super) fn declared_key(&mut self, a: &Automatic) -> Option<TyId> {
        a.attributes.clone().into_iter().find_map(|t| {
            let decl = self.declared_of(t, String::new())?;
            decl.fields
                .into_iter()
                .find(|f| f.name == "key")
                .map(|f| f.ty)
        })
    }

    /// The attributes object type `t` declares (`shown` in messages): an object type or record,
    /// also with named members; a dynamic type, or a union, whose shape depends on the value,
    /// takes anything. `None` for another type.
    fn declared_of(&mut self, t: TyId, shown: String) -> Option<Declared> {
        let core = self.cx.ty.core(t).unwrap_or(t);
        let open = self.cx.dynamic(core).is_some()
            || self.cx.union_members(core).is_some()
            || matches!(self.cx.ty.kind(core), TyKind::Param(_));
        if open {
            let rest = self.any_ty();
            return Some(Declared {
                fields: vec![],
                rest,
                shown,
            });
        }
        if let Some((base, tag)) = self.cx.record_view(core) {
            let rest = self.record_args(base).map(|(_, v)| v);
            let fields = self.declared_of(tag, String::new())?.fields;
            return Some(Declared {
                fields,
                rest,
                shown,
            });
        }
        if let Some((k, v)) = self.record_args(core) {
            if k == self.cx.ty.str_ {
                return Some(Declared {
                    fields: vec![],
                    rest: Some(v),
                    shown,
                });
            }
        }
        let (d, fields) = self.attr_fields(core)?;
        let defs = self.attr_flags(d);
        let mut out = vec![];
        for (i, (name, ty)) in fields.into_iter().enumerate() {
            let (optional, has_default, hidden) = defs.get(i).copied().unwrap_or_default();
            if hidden {
                continue;
            }
            let required = !optional && !has_default && self.cx.ty.opt_payload(ty).is_none();
            out.push(Field {
                name,
                ty,
                required,
                // An interface's methods come after its fields and have no field to go to.
                at: (i < defs.len()).then_some((d, i as u32)),
                also: None,
            });
        }
        Some(Declared {
            fields: out,
            rest: None,
            shown,
        })
    }

    /// What each field of object type or interface `d` is: (optional, has a default, hidden).
    fn attr_flags(&self, d: DefId) -> Vec<(bool, bool, bool)> {
        if let Some(i) = self.cx.iface(d) {
            return i
                .fields
                .iter()
                .map(|f| (f.optional, false, false))
                .collect();
        }
        (self.cx.adt(d))
            .map(|a| {
                let info = a.fields.iter();
                info.map(|f| {
                    let hidden = f.name.starts_with(ast::PRIVATE_NAME_PREFIX);
                    (f.optional, f.default.is_some(), hidden)
                })
                .collect()
            })
            .unwrap_or_default()
    }

    /// The members (name, type) of attributes type `t`: the fields of an object type, or those
    /// of an interface that is not one (it extends an empty interface or has methods), own and
    /// inherited, then its methods. The props object is built for the runtime's `props`
    /// parameter, so nothing here depends on how a value of `t` would be laid out.
    pub(super) fn attr_fields(&mut self, t: TyId) -> Option<(DefId, Vec<(String, TyId)>)> {
        let TyKind::Dyn(d, args) = self.cx.ty.kind(t).clone() else {
            return self.object_fields(t);
        };
        let i = self.cx.iface(d)?;
        let fields: Vec<(String, TyId)> = i.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
        let methods = i.methods.clone();
        let mut out: Vec<(String, TyId)> = fields
            .into_iter()
            .map(|(n, ty)| (n, self.cx.subst(ty, &args)))
            .collect();
        for m in methods {
            let ty = match (m.is_getter, m.generics.names.is_empty()) {
                (true, _) => m.ret,
                (false, true) => {
                    let params = m.params.iter().map(|p| p.ty).collect();
                    let throws = self.cx.ty.error;
                    self.cx.ty.intern(TyKind::FnPtr {
                        params,
                        ret: m.ret,
                        throws,
                        rest: false,
                    })
                }
                // A generic method has no function type to check a value against.
                (false, false) => match self.any_ty() {
                    Some(t) => t,
                    None => continue,
                },
            };
            out.push((m.name, self.cx.subst(ty, &args)));
        }
        Some((d, out))
    }
}
