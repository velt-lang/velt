#!/usr/bin/env python3
"""Temporary instrumentation for measuring a sample of the debug-variable test: it edits
lower/debug_vars.rs in a working copy so that mark(n) at each branch records which goldens reach it
(VELT_DBGCOV=<file>) and panics when VELT_MUT=n, so one build gives the program x branch matrix and
every branch-removal mutant. Apply it to a scratch checkout and don't commit the edited source; it
works on the file as #2407 found it, and stops on a pattern it no longer finds."""
import sys
p = sys.argv[1]
s = open(p, encoding="utf8").read()
R = [
 ("TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => DebugKind::Scalar(self.ty(t)),", "TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => { mark(1); DebugKind::Scalar(self.ty(t)) }"),
 ("TyKind::Str => DebugKind::Str,", "TyKind::Str => { mark(2); DebugKind::Str }"),
 ("_ if self.boxed(t) => DebugKind::Opaque(Ty::Ptr),", "_ if self.boxed(t) => { mark(3); DebugKind::Opaque(Ty::Ptr) }"),
 ("if a.kind == AdtKind::Class {\n                        DebugKind::Class {", "if a.kind == AdtKind::Class {\n                        mark(4);\n                        DebugKind::Class {"),
 ("} else {\n                        DebugKind::Struct {", "} else {\n                        mark(5);\n                        DebugKind::Struct {"),
 ("hir::Def::Enum(e) if self.is_c_like_enum(d) => DebugKind::Enum {", "hir::Def::Enum(e) if self.is_c_like_enum(d) => { mark(6); DebugKind::Enum {"),
 ("                        .collect(),\n                },\n                hir::Def::Enum(e) if e.is_union => {", "                        .collect(),\n                } }\n                hir::Def::Enum(e) if e.is_union => {\n                    mark(8);"),
 ("Some(_) => (v.name.clone(), i as i64),", "Some(_) => { mark(7); (v.name.clone(), i as i64) }"),
 ("hir::Def::Enum(e) => {\n                    let names = e.variants", "hir::Def::Enum(e) => {\n                    mark(9);\n                    let names = e.variants"),
 ("_ => DebugKind::Opaque(self.ty(t)),\n            },\n            TyKind::Result(..) => self.debug_tagged(t, vec![\"Ok\".into(), \"Err\".into()]),", "_ => { mark(10); DebugKind::Opaque(self.ty(t)) }\n            },\n            TyKind::Result(..) => { mark(11); self.debug_tagged(t, vec![\"Ok\".into(), \"Err\".into()]) }"),
 ("TyKind::Tuple(es) => {\n                let names = (0..es.len())", "TyKind::Tuple(es) => {\n                mark(12);\n                let names = (0..es.len())"),
 ("TyKind::Option(inner, _) => DebugKind::Option {", "TyKind::Option(inner, _) => { mark(13); DebugKind::Option {"),
 ("                inner: self.debug_ty(inner),\n            },\n            TyKind::Array(e) => DebugKind::Array {\n                elem: self.debug_ty(e),\n            },\n            TyKind::Shared(e) => DebugKind::Shared {\n                boxed: self.shared_box(e),\n                inner: self.debug_ty(e),\n            },\n            _ => DebugKind::Opaque(self.ty(t)),",
  "                inner: self.debug_ty(inner),\n            } }\n            TyKind::Array(e) => { mark(14); DebugKind::Array {\n                elem: self.debug_ty(e),\n            } }\n            TyKind::Shared(e) => { mark(15); DebugKind::Shared {\n                boxed: self.shared_box(e),\n                inner: self.debug_ty(e),\n            } }\n            _ => { mark(16); DebugKind::Opaque(self.ty(t)) }"),
 ("if !self.is_unit(ft) {\n                fields.push(DebugField {", "if self.is_unit(ft) { mark(18); }\n            if !self.is_unit(ft) {\n                mark(17);\n                fields.push(DebugField {"),
 ("if self.is_unit(p) {\n                    continue;", "if self.is_unit(p) {\n                    mark(19);\n                    continue;"),
 ("name: if union { \"value\".into() } else { k.to_string() },", "name: if union { mark(20); \"value\".into() } else { mark(21); k.to_string() },"),
 ("if let Some(name) = self.debug_types().names.get(&t) {\n            return name.clone();", "if let Some(name) = self.debug_types().names.get(&t) {\n            mark(22);\n            return name.clone();"),
 ("                    \"{ … }\".into()\n                }\n                _ => self.type_name(t),", "                    mark(23);\n                    \"{ … }\".into()\n                }\n                _ => { mark(24); self.type_name(t) }"),
 ("TyKind::Int(i) => super::keys::int_key(i).into(),", "TyKind::Int(i) => { mark(25); super::keys::int_key(i).into() }"),
 ("TyKind::Float(FloatTy::F64) => \"number\".into(),", "TyKind::Float(FloatTy::F64) => { mark(26); \"number\".into() }"),
 ("TyKind::Float(FloatTy::F32) => \"f32\".into(),", "TyKind::Float(FloatTy::F32) => { mark(27); \"f32\".into() }"),
 ("TyKind::Bool => \"boolean\".into(),", "TyKind::Bool => { mark(28); \"boolean\".into() }"),
 ("TyKind::Str => \"string\".into(),", "TyKind::Str => { mark(29); \"string\".into() }"),
 ("TyKind::Simd(s) => s.name().into(),", "TyKind::Simd(s) => { mark(30); s.name().into() }"),
 ("TyKind::Unit => \"void\".into(),", "TyKind::Unit => { mark(31); \"void\".into() }"),
 ("TyKind::Never => \"never\".into(),", "TyKind::Never => { mark(32); \"never\".into() }"),
 ("true => format!(\"({elem})[]\"),", "true => { mark(33); format!(\"({elem})[]\") }"),
 ("false => format!(\"{elem}[]\"),", "false => { mark(34); format!(\"{elem}[]\") }"),
 ("TyKind::Map(k, v) => self.debug_name_args(\"Map\".into(), &[k, v]),", "TyKind::Map(k, v) => { mark(35); self.debug_name_args(\"Map\".into(), &[k, v]) }"),
 ("TyKind::Tuple(es) => format!(\"[{}]\", self.debug_names(&es, \", \")),", "TyKind::Tuple(es) => { mark(36); format!(\"[{}]\", self.debug_names(&es, \", \")) }"),
 ("TyKind::Option(x, n) => format!(\"{}{}\", self.debug_name(x), n.suffix()),", "TyKind::Option(x, n) => { mark(37); format!(\"{}{}\", self.debug_name(x), n.suffix()) }"),
 ("TyKind::Result(a, b) => self.debug_name_args(\"Result\".into(), &[a, b]),", "TyKind::Result(a, b) => { mark(38); self.debug_name_args(\"Result\".into(), &[a, b]) }"),
 ("TyKind::Promise(x, _) => self.debug_name_args(\"Promise\".into(), &[x]),", "TyKind::Promise(x, _) => { mark(39); self.debug_name_args(\"Promise\".into(), &[x]) }"),
 ("TyKind::Shared(x) => self.debug_name_args(\"shared\".into(), &[x]),", "TyKind::Shared(x) => { mark(40); self.debug_name_args(\"shared\".into(), &[x]) }"),
 ("TyKind::FnPtr { .. } | TyKind::Closure(_) => \"function\".into(),", "TyKind::FnPtr { .. } | TyKind::Closure(_) => { mark(41); \"function\".into() }"),
 ("hir::Def::Enum(e) if e.is_union => {\n                    let n = e.variants.len() as u32;\n                    let members", "hir::Def::Enum(e) if e.is_union => {\n                    mark(42);\n                    let n = e.variants.len() as u32;\n                    let members"),
 ("hir::Def::Adt(a) if a.kind == AdtKind::Anon => {\n                    let names", "hir::Def::Adt(a) if a.kind == AdtKind::Anon => {\n                    mark(43);\n                    let names"),
 ("_ => {\n                    let name = self.type_name(t);", "_ => {\n                    mark(44);\n                    let name = self.type_name(t);"),
 ("TyKind::Dyn(_, args) => {\n                let name", "TyKind::Dyn(_, args) => {\n                mark(45);\n                let name"),
 ("_ => self.type_key(t),", "_ => { mark(46); self.type_key(t) }"),
 ("[] => name,", "[] => { mark(47); name }"),
 ("_ => format!(\"{name}<{}>\", self.debug_names(args, \", \")),", "_ => { mark(48); format!(\"{name}<{}>\", self.debug_names(args, \", \")) }"),
 ("hir::StmtKind::Let { local, .. } => found.push((*local, b.span)),", "hir::StmtKind::Let { local, .. } => { mark(49); found.push((*local, b.span)) }"),
 ("hir::StmtKind::LetPat { pat, .. } => {\n", "hir::StmtKind::LetPat { pat, .. } => {\n                    mark(50);\n"),
 ("hir::StmtKind::ForOf { binding, body, .. } => {\n", "hir::StmtKind::ForOf { binding, body, .. } => {\n                    mark(51);\n"),
 ("} => found.push((*l, handler.span)),", "} => { mark(52); found.push((*l, handler.span)) }"),
 ("let span = if i < f.captures.len() {\n", "let span = if i < f.captures.len() {\n                mark(53);\n"),
 ("            let debug = LocalDebug {\n", "            if by_ref { mark(54); }\n            if (f.captures.len()..params).contains(&i) { mark(56); }\n            if self.scope_ends.contains_key(&hir::LocalId(i as u32)) { mark(57); }\n            let debug = LocalDebug {\n"),
 (".starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$')\n            {\n                continue;", ".starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$')\n            {\n                mark(55);\n                continue;"),
 ("P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => {\n", "P::Variant { args: ps, .. } | P::Tuple(ps) | P::Or(ps) => {\n            mark(58);\n"),
 ("P::Adt { fields } => fields.iter().for_each(|(_, p)| pat_locals(p, f)),", "P::Adt { fields } => { mark(59); fields.iter().for_each(|(_, p)| pat_locals(p, f)) }"),
 ("P::Array { elems, rest } => {\n", "P::Array { elems, rest } => {\n            mark(60);\n"),
 ("if let Some(l) = rest {\n                f(*l);", "if let Some(l) = rest {\n                mark(61);\n                f(*l);"),
 ("P::Some(p) => pat_locals(p, f),", "P::Some(p) => { mark(62); pat_locals(p, f) }"),
 # Data mutants the verifier must see.
 ("index: self.vir_field(t, i as u32),", "index: self.vir_field(t, i as u32) + if mutant(100) { 100000 } else { 0 },"),
 ("index: fields.len() as u32 + 1,", "index: fields.len() as u32 + 1 + if mutant(101) { 100000 } else { 0 },"),
 ("by_ref,\n", "by_ref: by_ref || mutant(102),\n"),
 ("ty: self.debug_ty(ft),", "ty: if mutant(103) { DebugTyId(u32::MAX) } else { self.debug_ty(ft) },"),
]
for a, b in R:
    if s.count(a) != 1:
        sys.exit(f"pattern found {s.count(a)} times: {a!r}")
    s = s.replace(a, b)
s += r'''
fn mutant(n: u32) -> bool {
    static M: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("VELT_MUT").ok().and_then(|v| v.parse().ok())) == Some(n)
}

fn mark(n: u32) {
    use std::io::Write;
    use std::sync::Mutex;
    static SEEN: Mutex<u128> = Mutex::new(0);
    if mutant(n) {
        ice(&format!("mutant {n}"));
    }
    let mut seen = SEEN.lock().unwrap();
    if *seen & (1 << n) == 0 {
        *seen |= 1 << n;
        if let Some(path) = std::env::var_os("VELT_DBGCOV") {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
                let _ = writeln!(f, "{n}");
            }
        }
    }
}
'''
open(p, "w", encoding="utf8", newline="\n").write(s)
