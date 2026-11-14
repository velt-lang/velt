//! A spelling of a type that is the same in every program (#2943 phase 1, "Stable identity" in
//! docs/internals/design/build-architecture.md): by structure and by qualified name, never by
//! `TyId`, `DefId`, alias name or load order. Unions order their members by it
//! (`crate::unions`), so a union std writes has the same variants, in the same order, in every
//! program; before, members were ordered by interned id, which depends on what the program
//! interned first.
//!
//! Two types with the same spelling are the same type except for two named types with the same
//! qualified name (which modules can't declare) and a type that reaches itself through a union
//! (`type Json = … | Json[]`), spelled `^` inside itself. Unions tie-break on the interned id in
//! those cases only.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, FloatTy, IntTy, LitValue, Nothing, TyId, TyKind};

impl Ctx<'_> {
    /// `t` spelled the same in every program (module docs). Types don't change once interned,
    /// so each is spelled once (`Ctx::canon_keys`).
    pub(crate) fn canon_key(&mut self, t: TyId) -> std::rc::Rc<str> {
        if let Some(k) = self.canon_keys.get(&t) {
            return k.clone();
        }
        let mut out = String::new();
        self.canon_into(t, &mut out, &mut Vec::new());
        let k: std::rc::Rc<str> = out.into();
        self.canon_keys.insert(t, k.clone());
        k
    }

    fn canon_into(&self, t: TyId, out: &mut String, unions: &mut Vec<DefId>) {
        let list = |s: &Self, ts: &[TyId], out: &mut String, unions: &mut Vec<DefId>| {
            out.push('<');
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                s.canon_into(*t, out, unions);
            }
            out.push('>');
        };
        match self.ty.kind(t) {
            // The built-in types first (`!` sorts before every other spelling), in the order
            // `Types::new` makes them, so a union of them reads as it always has: `boolean |
            // string | f64`, `string | i64` (TypeScript also shows `string | number`).
            TyKind::Int(i) => out.push_str(&format!("!{:02}", int_rank(*i))),
            TyKind::Float(f) => out.push_str(&format!("!{:02}", float_rank(*f))),
            TyKind::Bool => out.push_str("!03"),
            TyKind::Str => out.push_str("!04"),
            TyKind::Unit => out.push_str("!00"),
            TyKind::Never => out.push_str("!01"),
            TyKind::Error => out.push_str("!02"),
            TyKind::Symbol => out.push_str("symbol"),
            TyKind::Simd(s) => out.push_str(s.name()),
            TyKind::Param(i) => out.push_str(&format!("${i}")),
            TyKind::Literal(v) => match v {
                LitValue::Str(s) => out.push_str(&format!("{s:?}")),
                LitValue::Int(i, n) => out.push_str(&format!("{n}{i:?}")),
                LitValue::Float(f, bits) => out.push_str(&format!("{bits:x}{f:?}")),
                LitValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            },
            TyKind::Array(e) => {
                out.push_str("[]");
                self.canon_into(*e, out, unions);
            }
            TyKind::Map(k, v) => {
                out.push_str("Map");
                list(self, &[*k, *v], out, unions);
            }
            TyKind::Tuple(ts) => {
                out.push_str("tuple");
                list(self, ts, out, unions);
            }
            TyKind::Option(t, n) => {
                out.push_str(match n {
                    Nothing::Null => "null|",
                    Nothing::Undefined => "undefined|",
                    Nothing::Both => "null|undefined|",
                });
                self.canon_into(*t, out, unions);
            }
            TyKind::Result(a, b) => {
                out.push_str("Result");
                list(self, &[*a, *b], out, unions);
            }
            TyKind::Promise(a, b) => {
                out.push_str("Promise");
                list(self, &[*a, *b], out, unions);
            }
            TyKind::Shared(t) => {
                out.push_str("shared");
                list(self, &[*t], out, unions);
            }
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => {
                out.push_str(if *rest { "fn..." } else { "fn" });
                list(self, params, out, unions);
                list(self, &[*ret, *throws], out, unions);
            }
            TyKind::Closure(d) => {
                out.push_str("closure ");
                out.push_str(&self.canon_def_name(*d));
            }
            TyKind::Dyn(d, args) => {
                out.push_str("dyn ");
                out.push_str(&self.canon_def_name(*d));
                list(self, args, out, unions);
            }
            TyKind::Op(op, args) => {
                out.push_str(&format!("op {op:?}"));
                list(self, args, out, unions);
            }
            TyKind::Adt(d, args) => self.canon_adt(*d, args, out, unions),
        }
    }

    fn canon_adt(&self, d: DefId, args: &[TyId], out: &mut String, unions: &mut Vec<DefId>) {
        match self.info.get(d.0 as usize) {
            // A union is its members, in canonical order: `A | B` is `B | A`.
            Some(DefInfo::Enum(e)) if e.is_union => {
                if unions.contains(&d) {
                    out.push('^');
                    return;
                }
                unions.push(d);
                let mut members: Vec<String> = (e.variants.iter())
                    .map(|v| {
                        let mut s = String::new();
                        self.canon_into(v.payload[0], &mut s, unions);
                        s
                    })
                    .collect();
                unions.pop();
                members.sort();
                out.push('(');
                out.push_str(&members.join("|"));
                out.push(')');
            }
            // An anonymous object type is its fields, in order.
            Some(DefInfo::Adt(a)) if a.kind == crate::hir::AdtKind::Anon => {
                out.push('{');
                for f in &a.fields {
                    // Quoted, so a name holding `:` or `;` can't read as two fields.
                    out.push_str(&format!("{:?}", f.name));
                    out.push(':');
                    self.canon_into(f.ty, out, unions);
                    out.push(';');
                }
                out.push('}');
            }
            _ => out.push_str(&self.canon_def_name(d)),
        }
        if !args.is_empty() {
            out.push('<');
            for (i, t) in args.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                self.canon_into(*t, out, unions);
            }
            out.push('>');
        }
    }

    /// A definition's qualified name, which modules make unique.
    fn canon_def_name(&self, d: DefId) -> String {
        match self.info.get(d.0 as usize) {
            Some(DefInfo::Adt(a)) => a.qual_name.clone(),
            Some(DefInfo::Enum(e)) => e.qual_name.clone(),
            Some(DefInfo::Iface(i)) => i.qual_name.clone(),
            Some(DefInfo::Fn(f)) => f.name.clone(),
            Some(DefInfo::Global(g)) => g.qual_name.clone(),
            // Not a definition sema made (an internal error elsewhere): no name to spell.
            None => "?".to_string(),
        }
    }
}

/// Where an integer type sorts among the built-in types (`Ctx::canon_into`): `i32`, `i64`, `u64`
/// and `usize` as `Types::new` makes them, the others after every built-in.
fn int_rank(i: IntTy) -> u32 {
    match i {
        IntTy::I32 => 5,
        IntTy::I64 => 6,
        IntTy::U64 => 7,
        IntTy::USize => 8,
        IntTy::I8 => 20,
        IntTy::I16 => 21,
        IntTy::ISize => 22,
        IntTy::U8 => 23,
        IntTy::U16 => 24,
        IntTy::U32 => 25,
    }
}

/// [`int_rank`] for float types: `f64` with the built-ins.
fn float_rank(f: FloatTy) -> u32 {
    match f {
        FloatTy::F64 => 9,
        FloatTy::F32 => 30,
        FloatTy::F16 => 31,
        FloatTy::BF16 => 32,
    }
}
