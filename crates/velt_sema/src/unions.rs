//! Union types `A | B | C` (docs/reference/types.md "Union types", hir_encodings.md): each distinct
//! member set is a compiler-generated enum (`EnumDef::is_union`) with one single-payload variant
//! per member.
//!
//! Canonical form: nested unions and `| null` are flattened, members deduplicated, `never`
//! dropped and sorted (concrete types first, then by interned id), so `A | B` and `B | A` are the
//! same type. A single non-null member is that type itself (`T | null` stays `Option<T>`);
//! `null` together with several members is `Option<union>`. Like anonymous object types, the enum
//! is generic over the type parameters its members mention (renumbered by first occurrence).

use std::collections::HashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{DefInfo, EnumInfo, Generics, VariantInfo};
use crate::hir::{DefId, TyId, TyKind};
use crate::types::collect_params;

/// The JS `typeof` tags Velt supports, in the order used by diagnostics.
pub(crate) const TYPEOF_TAGS: [&str; 5] = ["string", "number", "boolean", "object", "function"];

impl Ctx<'_> {
    /// The canonical type of the union of `members` (plus `null` when `nullable`); reports
    /// invalid members at `span` and returns `Error` for them.
    pub fn union_of(&mut self, members: &[TyId], nullable: bool, span: Span) -> TyId {
        let mut flat = vec![];
        let mut nullable = nullable;
        for &m in members {
            self.flatten_member(m, &mut flat, &mut nullable);
        }
        if flat.contains(&self.ty.error) {
            return self.ty.error;
        }
        if flat.contains(&self.ty.unit) {
            self.err("`void` cannot be a member of a union type", span);
            return self.ty.error;
        }
        let never = self.ty.never;
        flat.retain(|t| *t != never);
        let mut members: Vec<TyId> = vec![];
        for t in flat {
            if !members.contains(&t) {
                members.push(t);
            }
        }
        let ty = &self.ty;
        members.sort_by_key(|t| (has_params(ty, *t), t.0));
        let core = match members.len() {
            0 if nullable => {
                self.err("`null` alone is not a type; write `T | null`", span);
                return self.ty.error;
            }
            0 => return never,
            1 => members[0],
            _ => self.union_enum(&members),
        };
        if nullable {
            self.ty.option(core)
        } else {
            core
        }
    }

    fn flatten_member(&mut self, t: TyId, out: &mut Vec<TyId>, nullable: &mut bool) {
        if let Some(p) = self.ty.opt_payload(t) {
            *nullable = true;
            return self.flatten_member(p, out, nullable);
        }
        match self.union_members(t) {
            Some(ms) => out.extend(ms),
            None => out.push(t),
        }
    }

    /// `(union enum, type args)` if `t` is a union type.
    pub fn union_def(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.enum_info(*d).is_some_and(|e| e.is_union) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Member types of union type `t` in variant order (`None` if `t` is not a union).
    pub fn union_members(&mut self, t: TyId) -> Option<Vec<TyId>> {
        let (d, args) = self.union_def(t)?;
        let payloads: Vec<TyId> = self
            .enum_info(d)?
            .variants
            .iter()
            .map(|v| v.payload[0])
            .collect();
        Some(payloads.into_iter().map(|p| self.subst(p, &args)).collect())
    }

    /// What JS `typeof` answers for a (non-null) value of type `t`: every number type is
    /// `"number"`, closures `"function"`, classes / structs / arrays / maps / … `"object"`.
    pub fn typeof_tag(&self, t: TyId) -> &'static str {
        match self.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) => "number",
            TyKind::Str => "string",
            TyKind::Bool => "boolean",
            TyKind::FnPtr { .. } | TyKind::Closure(_) => "function",
            TyKind::Unit => "undefined",
            TyKind::Literal(v) => match v {
                crate::hir::LitValue::Str(_) => "string",
                crate::hir::LitValue::Bool(_) => "boolean",
                _ => "number",
            },
            _ => "object",
        }
    }

    /// Is `t` an instance type of class `class` or of one of its subclasses?
    pub fn is_instance_of(&self, t: TyId, class: DefId) -> bool {
        let mut cur = self.class_of(t).map(|(d, _)| d);
        for _ in 0..64 {
            match cur {
                Some(d) if d == class => return true,
                Some(d) => {
                    cur = self
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.class_of(b))
                        .map(|(d, _)| d)
                }
                None => return false,
            }
        }
        false
    }

    /// The union enum over canonical (sorted, distinct) `members`, applied to their params.
    fn union_enum(&mut self, members: &[TyId]) -> TyId {
        let mut params: Vec<u32> = vec![];
        for m in members {
            collect_params(&self.ty, *m, &mut params);
        }
        let renumber: HashMap<u32, TyId> = params
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.ty.param(i as u32)))
            .collect();
        let norm: Vec<TyId> = members
            .iter()
            .map(|t| {
                self.ty.map(*t, &mut |k| match k {
                    TyKind::Param(i) => renumber.get(i).copied(),
                    _ => None,
                })
            })
            .collect();
        let args: Vec<TyId> = params.iter().map(|p| self.ty.param(*p)).collect();
        let d = match self.unions.get(&norm) {
            Some(d) => *d,
            None => self.new_union_def(norm, params.len()),
        };
        self.ty.intern(TyKind::Adt(d, args))
    }

    fn new_union_def(&mut self, norm: Vec<TyId>, n: usize) -> DefId {
        let mut generics = Generics::default();
        for i in 0..n {
            generics.push(&format!("T{i}"));
        }
        let saved = std::mem::replace(&mut self.display_params, generics.names.clone());
        let names: Vec<String> = norm.iter().map(|t| self.display(*t)).collect();
        self.display_params = saved;
        let name = names.join(" | ");
        let variants = names
            .into_iter()
            .zip(&norm)
            .enumerate()
            .map(|(i, (name, t))| VariantInfo {
                name,
                payload: vec![*t],
                discriminant: i as i64,
                str_value: None,
            })
            .collect();
        let info = EnumInfo {
            name: name.clone(),
            qual_name: name,
            span: Span::DUMMY,
            generics,
            variants,
            is_union: true,
            decl: None,
        };
        let d = self.alloc_def(Span::DUMMY, DefInfo::Enum(Box::new(info)));
        self.unions.insert(norm, d);
        d
    }

    /// `A | B` for diagnostics: the members of union `d` applied to `args` (which mention the
    /// parameters in scope, named `params`).
    pub(crate) fn display_union(&self, d: DefId, args: &[TyId], params: &[String]) -> String {
        let Some(e) = self.enum_info(d) else {
            return "unknown".into();
        };
        let names: Vec<String> = args.iter().map(|a| self.display_in(*a, params)).collect();
        let parts: Vec<String> = e
            .variants
            .iter()
            .map(|v| self.display_applied(v.payload[0], &names))
            .collect();
        parts.join(" | ")
    }

    /// Display `t` with `Param(i)` named `names[i]`, parenthesizing arrays of parameters.
    fn display_applied(&self, t: TyId, names: &[String]) -> String {
        match self.ty.kind(t) {
            TyKind::Array(e) if has_params(&self.ty, *e) => {
                format!("({})[]", self.display_applied(*e, names))
            }
            _ => self.display_in(t, names),
        }
    }
}

fn has_params(ty: &crate::types::Types, t: TyId) -> bool {
    let mut ps = vec![];
    collect_params(ty, t, &mut ps);
    !ps.is_empty()
}
