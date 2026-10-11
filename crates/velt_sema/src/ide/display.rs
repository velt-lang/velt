//! Types as Velt source spells them, from an owned snapshot of the type table and the names of
//! the type definitions (so queries work after checking is done): `Map<string, i64>`, `Point`,
//! `{ x: i64 }`, `(x: i64) => string`, generic parameters by their declared names.

use std::collections::HashMap;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{FloatTy, TyId, TyKind, TyTable};
use crate::types::int_name;

/// How a type definition is displayed.
#[derive(Clone, Debug)]
pub(crate) enum TypeName {
    Named(String),
    /// Anonymous object: its fields.
    Anon(Vec<(String, TyId)>),
    /// A closure: its declared params (names and types) and result.
    Closure(Vec<(String, TyId)>, TyId),
    /// Not a type definition.
    None,
}

/// Owned data needed to display types.
pub(crate) struct Names {
    pub table: TyTable,
    pub defs: Vec<TypeName>,
    /// Type alias names of structural types (`Ctx::alias_names`).
    pub aliases: HashMap<TyId, String>,
}

impl Names {
    pub fn capture(cx: &Ctx) -> Names {
        let defs = cx.info.iter().map(type_name).collect();
        Names {
            table: cx.ty.table.clone(),
            defs,
            aliases: cx.alias_names.clone(),
        }
    }

    /// Display `t`; `param(i)` spells generic parameter `i`.
    pub fn show(&self, t: TyId, param: &dyn Fn(u32) -> String) -> String {
        let list = |ts: &[TyId]| {
            ts.iter()
                .map(|t| self.show(*t, param))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let with_args = |name: &str, args: &[TyId]| {
            if args.is_empty() {
                name.to_string()
            } else {
                format!("{name}<{}>", list(args))
            }
        };
        if let Some(n) = self.aliases.get(&t) {
            return n.clone();
        }
        match self.table.kind(t) {
            TyKind::Int(i) => int_name(*i).to_string(),
            TyKind::Float(FloatTy::F32) => "f32".into(),
            TyKind::Float(FloatTy::F64) => "f64".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Str => "string".into(),
            TyKind::Symbol => "symbol".into(),
            TyKind::Unit => "void".into(),
            TyKind::Never => "never".into(),
            TyKind::Error => "unknown".into(),
            TyKind::Literal(v) => crate::literals::display_lit(v),
            TyKind::Array(e) => match self.table.kind(*e) {
                TyKind::Option(_) | TyKind::FnPtr { .. } => format!("({})[]", self.show(*e, param)),
                _ => format!("{}[]", self.show(*e, param)),
            },
            TyKind::Map(k, v) => with_args("Map", &[*k, *v]),
            TyKind::Tuple(ts) => format!("[{}]", list(ts)),
            TyKind::Option(x) => format!("{} | null", self.show(*x, param)),
            TyKind::Result(a, b) => with_args("Result", &[*a, *b]),
            TyKind::Promise(x, e) if matches!(self.table.kind(*e), TyKind::Never) => {
                with_args("Promise", &[*x])
            }
            TyKind::Promise(x, e) => with_args("Promise", &[*x, *e]),
            TyKind::Shared(x) => with_args("shared", &[*x]),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => {
                let ps: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("arg{i}: {}", self.show(*p, param)))
                    .collect();
                let th = match self.table.kind(*throws) {
                    TyKind::Never => String::new(),
                    _ => format!(" throws {}", self.show(*throws, param)),
                };
                format!("({}) => {}{th}", ps.join(", "), self.show(*ret, param))
            }
            TyKind::Adt(d, args) | TyKind::Dyn(d, args) => match &self.defs[d.0 as usize] {
                TypeName::Named(n) => with_args(n, args),
                TypeName::Anon(fields) => {
                    let inner = |i: u32| {
                        args.get(i as usize)
                            .map_or_else(|| param(i), |a| self.show(*a, param))
                    };
                    let fs: Vec<String> = fields
                        .iter()
                        .map(|(n, t)| format!("{n}: {}", self.show(*t, &inner)))
                        .collect();
                    format!("{{ {} }}", fs.join("; "))
                }
                _ => "unknown".into(),
            },
            TyKind::Closure(d) => match &self.defs[d.0 as usize] {
                TypeName::Closure(ps, ret) => self.signature(ps, *ret, param),
                _ => "unknown".into(),
            },
            TyKind::Param(n) => param(*n),
        }
    }

    /// `(a: A, b: B) => R`.
    pub fn signature(
        &self,
        ps: &[(String, TyId)],
        ret: TyId,
        param: &dyn Fn(u32) -> String,
    ) -> String {
        let ps: Vec<String> = ps
            .iter()
            .map(|(n, t)| format!("{n}: {}", self.show(*t, param)))
            .collect();
        format!("({}) => {}", ps.join(", "), self.show(ret, param))
    }

    /// Display with generic parameters named by `names`.
    pub fn show_in(&self, t: TyId, names: &[String]) -> String {
        self.show(t, &|i| named_param(names, i))
    }
}

/// Parameter `i` of a generic context named `names` (`T3` if unnamed).
pub(crate) fn named_param(names: &[String], i: u32) -> String {
    names
        .get(i as usize)
        .cloned()
        .unwrap_or_else(|| format!("T{i}"))
}

fn type_name(d: &DefInfo) -> TypeName {
    match d {
        DefInfo::Adt(a) if a.kind == crate::hir::AdtKind::Anon => {
            TypeName::Anon(a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect())
        }
        DefInfo::Adt(a) => TypeName::Named(a.name.clone()),
        DefInfo::Enum(e) => TypeName::Named(e.name.clone()),
        DefInfo::Iface(i) => TypeName::Named(i.name.clone()),
        // Function values display with their parameter names (the IDE records a
        // `TyKind::Closure(def)` type for closures and named functions used as values).
        DefInfo::Fn(f) => {
            let ps = f.params.iter().map(|p| (p.name.clone(), p.ty)).collect();
            TypeName::Closure(ps, f.ret)
        }
        DefInfo::Global(_) => TypeName::None,
    }
}
