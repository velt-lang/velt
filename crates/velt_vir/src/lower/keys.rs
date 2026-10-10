//! Stable spellings of monomorphic types for symbols (docs/internals/design/hot-reload.md, "Stable
//! function keys"). `velt dev` matches functions across program versions by symbol, so a symbol
//! must not depend on `TyId`s, which are intern indices that shift with unrelated edits: generic
//! instances spell out their type arguments (`sum<i64>` → `_V3sum_T3i64`), glue its type
//! (`_Gdrop_5Point`).
//!
//! The spelling is injective for the types lowering sees: ADTs, interfaces and closures are named
//! by their fully qualified HIR names (unique per definition), everything else structurally.

use std::collections::HashMap;

use velt_sema::hir::{DefId, FloatTy, IntTy, LitValue, TyId, TyKind};

use super::Cx;
use crate::mangle::{mangle, segment};
use crate::vir::Function;

impl Cx<'_> {
    /// The stable spelling of type `t`, e.g. `i64`, `Map<string, Point[]>`, `Box<i64>`.
    pub(super) fn type_key(&self, t: TyId) -> String {
        self.type_key_in(t, &[], &mut vec![])
    }

    /// [`type_key`](Self::type_key) with `Param(n)` spelled `env[n]` (the field types of an
    /// anonymous def, over its own parameters).
    /// `stack`: the anonymous defs being spelled, so a recursive shape (a field-only interface
    /// `Tree { children: Tree[] }`) names itself instead of unfolding forever.
    fn type_key_in(&self, t: TyId, env: &[String], stack: &mut Vec<DefId>) -> String {
        let list = |ts: &[TyId], stack: &mut Vec<DefId>| {
            ts.iter()
                .map(|t| self.type_key_in(*t, env, stack))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self.types.kind(t) {
            TyKind::Int(i) => int_key(*i).into(),
            TyKind::Float(FloatTy::F32) => "f32".into(),
            TyKind::Float(FloatTy::F64) => "f64".into(),
            TyKind::Bool => "bool".into(),
            TyKind::Str => "string".into(),
            TyKind::Unit => "void".into(),
            TyKind::Never => "never".into(),
            TyKind::Error => "error".into(),
            // Anonymous object types are structural: one shape can be several defs (a generic
            // def's instance, sema's concrete def, Cx::canon), and its symbols must not depend
            // on which one a program happens to use.
            TyKind::Adt(d, args) if self.is_anon_def(*d) && !stack.contains(d) => {
                let env: Vec<String> = args
                    .iter()
                    .map(|a| self.type_key_in(*a, env, stack))
                    .collect();
                let velt_sema::hir::Def::Adt(a) = self.hir.def(*d) else {
                    unreachable!("anonymous def")
                };
                stack.push(*d);
                let fs: Vec<String> = a
                    .fields
                    .iter()
                    .map(|f| {
                        // `??` marks a presence field (`d?: T | null`), whose layout differs
                        // from `d?: T`'s although both read as `Option<T>`.
                        let q = match (f.optional, f.presence) {
                            (_, true) => "??",
                            (true, false) => "?",
                            (false, false) => "",
                        };
                        format!("{}{q}: {}", f.name, self.type_key_in(f.ty, &env, stack))
                    })
                    .collect();
                stack.pop();
                format!("{{ {} }}", fs.join("; "))
            }
            // Unions likewise: their members, sorted (a generic union's instance and the written
            // union are one type, Cx::canon).
            TyKind::Adt(d, args) if self.is_union_def(*d) => {
                let env: Vec<String> = args
                    .iter()
                    .map(|a| self.type_key_in(*a, env, stack))
                    .collect();
                let velt_sema::hir::Def::Enum(e) = self.hir.def(*d) else {
                    unreachable!("union def")
                };
                let mut ms: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| self.type_key_in(v.payload[0], &env, stack))
                    .collect();
                ms.sort();
                ms.join(" | ")
            }
            TyKind::Adt(d, args) => with_args(&self.def_name(*d), &list(args, stack)),
            TyKind::Dyn(d, args) => {
                with_args(&format!("dyn {}", self.def_name(*d)), &list(args, stack))
            }
            TyKind::Array(e) => format!("{}[]", self.type_key_in(*e, env, stack)),
            TyKind::Map(k, v) => format!("Map<{}>", list(&[*k, *v], stack)),
            TyKind::Tuple(ts) => format!("[{}]", list(ts, stack)),
            TyKind::Option(e) => format!("Option<{}>", self.type_key_in(*e, env, stack)),
            TyKind::Result(a, b) => format!("Result<{}>", list(&[*a, *b], stack)),
            TyKind::Promise(t, e) => format!("Promise<{}>", list(&[*t, *e], stack)),
            TyKind::Shared(e) => format!("shared<{}>", self.type_key_in(*e, env, stack)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => format!(
                "fn({}) => {} throws {}",
                list(params, stack),
                self.type_key_in(*ret, env, stack),
                self.type_key_in(*throws, env, stack)
            ),
            TyKind::Closure(d) => format!("closure {}", self.def_name(*d)),
            TyKind::Param(n) => env
                .get(*n as usize)
                .cloned()
                .unwrap_or_else(|| format!("${n}")),
            TyKind::Literal(v) => lit_key(v),
        }
    }

    /// Link symbol of a function instance: the mangled name, plus `_T` and the escaped,
    /// comma-separated type arguments (`_` never follows a mangled segment, so instances cannot
    /// collide with other names).
    pub(super) fn instance_symbol(&self, name: &str, targs: &[TyId]) -> String {
        let mut s = mangle(name);
        if !targs.is_empty() {
            s.push_str("_T");
            s.push_str(&segment(&self.type_args_key(targs)));
        }
        s
    }

    /// Symbol suffix naming type `t`: its escaped stable spelling.
    pub(super) fn type_symbol(&self, t: TyId) -> String {
        segment(&self.type_key(t))
    }

    fn type_args_key(&self, targs: &[TyId]) -> String {
        targs
            .iter()
            .map(|t| self.type_key(*t))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn is_union_def(&self, d: velt_sema::hir::DefId) -> bool {
        matches!(self.hir.def(d), velt_sema::hir::Def::Enum(e) if e.is_union)
    }

    fn is_anon_def(&self, d: velt_sema::hir::DefId) -> bool {
        matches!(self.hir.def(d), velt_sema::hir::Def::Adt(a)
            if a.kind == velt_sema::hir::AdtKind::Anon)
    }

    fn def_name(&self, d: velt_sema::hir::DefId) -> String {
        use velt_sema::hir::Def;
        match self.hir.def(d) {
            Def::Adt(a) => a.name.clone(),
            Def::Enum(e) => e.name.clone(),
            Def::Interface(i) => i.name.clone(),
            Def::Fn(f) => f.name.clone(),
            _ => format!("def{}", d.0),
        }
    }
}

/// Rename repeated symbols `s` to `s$dup2`, `s$dup3`, … (lowering never spells `$dup`
/// otherwise). Two functions only share a symbol if two definitions share a qualified name; this
/// keeps symbols unique (as backends need) regardless, and `velt dev` treats such keys as
/// ambiguous.
pub(super) fn disambiguate(funcs: &mut [Function]) {
    let mut seen: HashMap<String, u32> = HashMap::new();
    for f in funcs {
        let n = seen.entry(f.symbol.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            f.symbol = format!("{}$dup{n}", f.symbol);
        }
    }
}

fn with_args(name: &str, args: &str) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        format!("{name}<{args}>")
    }
}

pub(super) fn int_key(i: IntTy) -> &'static str {
    match i {
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

fn lit_key(v: &LitValue) -> String {
    match v {
        LitValue::Str(s) => format!("{s:?}"),
        LitValue::Int(i, n) => format!("{n}{}", int_key(*i)),
        LitValue::Float(FloatTy::F32, bits) => format!("{bits:#x}f32"),
        LitValue::Float(FloatTy::F64, bits) => format!("{bits:#x}f64"),
        LitValue::Bool(b) => b.to_string(),
    }
}
