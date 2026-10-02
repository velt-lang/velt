//! Fingerprints of functions and layouts that do not depend on VIR ids, so two versions of a
//! program can be compared per function key (docs/internals/design/hot-reload.md, phase 3).
//!
//! VIR refers to functions, aggregates and statics by index, and indices shift with any edit.
//! A fingerprint therefore names functions and externs by symbol, statics by their contents
//! and aggregates by their structure (name, size, alignment, fields). The aggregates of async
//! state machines (`… state`, `… promise`) are the exception: they are named only. Their
//! layout follows from the poll function that owns them, and the code that embeds them is
//! recompiled through its reference to that poll function (`classify`), so a child's new
//! layout does not count as an edit of every function that awaits it (or of `main`).
//! Source locations are left out: JIT code has no debug info (panic locations are strings,
//! which are part of the statics). The location an error records for an `Uncaught …` report
//! (the argument of `velt_rt_set_throw_loc`) is left out too: nearly every call into the
//! standard library records one, so counting it would make an edit that only moves lines
//! change `main` and force a restart.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use velt_vir::vir::{
    AggId, Callee, Const, Function, Operand, Place, Program, Proj, Rvalue, StaticId, Stmt,
    Terminator, Ty,
};

/// A 128-bit fingerprint (two independent 64-bit hashes of the canonical text).
pub(crate) type Fingerprint = u128;

/// Is `name` the aggregate of an async state machine (pinned to its version, never compared)?
pub(crate) fn is_state_layout(name: &str) -> bool {
    name.ends_with(" state") || name.ends_with(" promise")
}

/// Canonical spellings of one program's aggregates and statics, computed once.
pub(crate) struct Canon<'p> {
    program: &'p Program,
    aggs: Vec<String>,
    statics: Vec<Fingerprint>,
}

impl<'p> Canon<'p> {
    /// Precompute the program's aggregate and static spellings.
    pub(crate) fn new(program: &'p Program) -> Self {
        let mut canon = Canon {
            program,
            aggs: vec![],
            statics: vec![],
        };
        canon.aggs = (0..program.aggs.len())
            .map(|i| canon.agg_text(AggId(i as u32), 0))
            .collect();
        canon.statics = (0..program.statics.len())
            .map(|i| canon.static_print(StaticId(i as u32), 0))
            .collect();
        canon
    }

    /// The structural spelling of aggregate `id` (state machines: their name only).
    pub(crate) fn agg(&self, id: AggId) -> &str {
        self.aggs.get(id.0 as usize).map_or("?", String::as_str)
    }

    /// The fingerprint of `func`: signature, locals and code.
    pub(crate) fn function(&self, func: &Function) -> Fingerprint {
        let mut out = String::new();
        let _ = write!(out, "fn(");
        func.params.iter().for_each(|t| self.ty(&mut out, *t));
        let _ = write!(out, ")");
        self.ty(&mut out, func.ret);
        for local in &func.locals {
            self.ty(&mut out, local.ty);
        }
        for (i, block) in func.blocks.iter().enumerate() {
            let _ = write!(out, "\nbb{i}:");
            for stmt in &block.stmts {
                self.stmt(&mut out, stmt);
            }
            self.term(&mut out, &block.term);
        }
        fingerprint(&out)
    }

    fn agg_text(&self, id: AggId, depth: u32) -> String {
        let Some(layout) = self.program.aggs.get(id.0 as usize) else {
            return format!("agg?{}", id.0);
        };
        if is_state_layout(&layout.name) || depth > 16 {
            return format!("{{{}}}", layout.name);
        }
        let mut out = format!("{{{} {}/{}", layout.name, layout.size, layout.align);
        for (ty, offset) in &layout.fields {
            match ty {
                Ty::Agg(inner) => {
                    let _ = write!(out, " {}@{offset}", self.agg_text(*inner, depth + 1));
                }
                scalar => {
                    let _ = write!(out, " {scalar:?}@{offset}");
                }
            }
        }
        out.push('}');
        out
    }

    fn static_print(&self, id: StaticId, depth: u32) -> Fingerprint {
        let Some(data) = self.program.statics.get(id.0 as usize) else {
            return 0;
        };
        let mut out = format!("{} {:?}", data.align, data.bytes);
        for (offset, target) in &data.relocs {
            let _ = write!(out, " @{offset}=");
            match target {
                // Static tables point at each other only shallowly (vtables, string objects).
                Const::Static(s) if depth < 8 => {
                    let _ = write!(out, "S{:x}", self.static_print(*s, depth + 1));
                }
                other => self.constant(&mut out, other),
            }
        }
        fingerprint(&out)
    }

    fn ty(&self, out: &mut String, ty: Ty) {
        match ty {
            Ty::Agg(id) => out.push_str(self.agg(id)),
            scalar => {
                let _ = write!(out, " {scalar:?}");
            }
        }
    }

    fn constant(&self, out: &mut String, c: &Const) {
        let _ = match c {
            Const::Int(v) => write!(out, " {v}"),
            Const::Float(v) => write!(out, " f{:x}", v.to_bits()),
            Const::Bool(b) => write!(out, " {b}"),
            Const::Unit => write!(out, " ()"),
            Const::Static(s) => write!(
                out,
                " S{:x}",
                self.statics.get(s.0 as usize).copied().unwrap_or(0)
            ),
            Const::Func(f) => write!(out, " F{}", self.func_name(f.0)),
            Const::Extern(e) => write!(out, " E{}", self.extern_name(e.0)),
        };
    }

    fn func_name(&self, id: u32) -> &str {
        self.program
            .funcs
            .get(id as usize)
            .map_or("?", |f| f.symbol.as_str())
    }

    fn extern_name(&self, id: u32) -> &str {
        self.program
            .externs
            .get(id as usize)
            .map_or("?", |e| e.symbol.as_str())
    }

    fn place(&self, out: &mut String, place: &Place) {
        let _ = write!(out, " _{}", place.local.0);
        for proj in &place.proj {
            match proj {
                Proj::Field(n) => {
                    let _ = write!(out, ".{n}");
                }
                Proj::Deref(ty) => {
                    out.push_str(".*");
                    self.ty(out, *ty);
                }
                Proj::Cast(id) => {
                    out.push_str(" as ");
                    out.push_str(self.agg(*id));
                }
            }
        }
    }

    fn operand(&self, out: &mut String, op: &Operand) {
        match op {
            Operand::Copy(place) => self.place(out, place),
            Operand::Const(c, ty) => {
                self.constant(out, c);
                self.ty(out, *ty);
            }
        }
    }

    fn rvalue(&self, out: &mut String, rv: &Rvalue) {
        match rv {
            Rvalue::Use(op) => self.operand(out, op),
            Rvalue::Unary(op, a) => {
                let _ = write!(out, " {op:?}");
                self.operand(out, a);
            }
            Rvalue::Binary(op, a, b) => {
                let _ = write!(out, " {op:?}");
                self.operand(out, a);
                self.operand(out, b);
            }
            Rvalue::Cast(a, ty) => {
                out.push_str(" cast");
                self.operand(out, a);
                self.ty(out, *ty);
            }
            Rvalue::AddrOf(place) => {
                out.push_str(" &");
                self.place(out, place);
            }
            Rvalue::Aggregate(id, ops) => {
                out.push_str(" agg ");
                out.push_str(self.agg(*id));
                ops.iter().for_each(|op| self.operand(out, op));
            }
        }
    }

    fn stmt(&self, out: &mut String, stmt: &Stmt) {
        out.push_str("\n ");
        match stmt {
            Stmt::Assign(place, rv) => {
                self.place(out, place);
                out.push_str(" =");
                self.rvalue(out, rv);
            }
            Stmt::MemCopy { dst, src, size } => {
                let _ = write!(out, "memcopy {size}");
                self.operand(out, dst);
                self.operand(out, src);
            }
            Stmt::MemCopyDyn {
                dst,
                src,
                len,
                overlapping,
            } => {
                let _ = write!(out, "memcopy {overlapping}");
                [dst, src, len].iter().for_each(|op| self.operand(out, op));
            }
            Stmt::MemSet { dst, byte, len } => {
                out.push_str("memset");
                [dst, byte, len].iter().for_each(|op| self.operand(out, op));
            }
            Stmt::Nop => {}
        }
    }

    fn term(&self, out: &mut String, term: &Terminator) {
        out.push_str("\n ");
        match term {
            Terminator::Goto(b) => {
                let _ = write!(out, "goto {}", b.0);
            }
            Terminator::Branch { cond, then, els } => {
                let _ = write!(out, "branch {} {}", then.0, els.0);
                self.operand(out, cond);
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let _ = write!(out, "switch {cases:?} {}", default.0);
                self.operand(out, value);
            }
            Terminator::Return(op) => {
                out.push_str("return");
                self.operand(out, op);
            }
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => self.call(out, callee, args, dest.as_ref(), next.0),
            Terminator::Unreachable => out.push_str("unreachable"),
        }
    }

    fn call(
        &self,
        out: &mut String,
        callee: &Callee,
        args: &[Operand],
        dest: Option<&Place>,
        next: u32,
    ) {
        let _ = write!(out, "call {next}");
        match callee {
            Callee::Func(f) => {
                let _ = write!(out, " F{}", self.func_name(f.0));
            }
            Callee::Extern(e) => {
                let name = self.extern_name(e.0);
                let _ = write!(out, " E{name}");
                if name == "velt_rt_set_throw_loc" {
                    return;
                }
            }
            Callee::Ptr {
                target,
                params,
                ret,
            } => {
                self.operand(out, target);
                params.iter().for_each(|t| self.ty(out, *t));
                self.ty(out, *ret);
            }
        }
        args.iter().for_each(|a| self.operand(out, a));
        if let Some(dest) = dest {
            out.push_str(" ->");
            self.place(out, dest);
        }
    }
}

/// Two independent 64-bit hashes of `text` (std's SipHash with fixed keys, so the result is the
/// same for the whole session).
pub(crate) fn fingerprint(text: &str) -> Fingerprint {
    let mut a = DefaultHasher::new();
    text.hash(&mut a);
    let mut b = DefaultHasher::new();
    0x5eed_u32.hash(&mut b);
    text.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}
