//! Fingerprints of functions and layouts that do not depend on VIR ids, so a function can be
//! compared with a function of another program or another version of the program.
//!
//! VIR refers to functions, aggregates and statics by index, and indices shift with any edit.
//! A fingerprint therefore names functions and externs by symbol, statics by their contents
//! and aggregates by their structure (name, size, alignment, fields); link mode also spells an
//! extern's declaration. Source locations and debug
//! variables are left out: they do not change what the code does (panic locations are strings,
//! which are part of the statics). Two [`Mode`]s:
//!
//! - [`Mode::HotReload`] (`velt dev`, docs/internals/design/hot-reload.md, phase 3). The
//!   aggregates of async state machines (`… state`, `… promise`) are named only: their layout
//!   follows from the poll function that owns them, and the code that embeds them is recompiled
//!   through its reference to that poll function (`classify`), so a child's new layout does not
//!   count as an edit of every function that awaits it (or of `main`). Aggregates nested deeper
//!   than 16 levels and statics deeper than 8 are named only, for speed. The location an error
//!   records for an `Uncaught …` report (the argument of `velt_rt_set_throw_loc`) is left out:
//!   nearly every call into the standard library records one, so counting it would make an edit
//!   that only moves lines change `main` and force a restart.
//! - [`Mode::Link`] (the cached std object, docs/internals/design/std-object.md): whether a
//!   function's code compiled elsewhere is right for this program. Nothing recompiles a function
//!   the other way, so every omission above is unsound there: state aggregates and every nesting
//!   level count by structure, statics by their whole relocation graph, and the throw location
//!   is part of the code, and so are whether the function is a poll function, its gated target
//!   and every extern's declaration (signature and `noreturn`, which LLVM declares on it and
//!   lowering keeps from the first declaration of the symbol). The parameter
//!   attributes callers uphold (vir.rs invariant 9) are compared apart ([`attrs_hold`]): they
//!   depend on the whole program (a type module state can reach loses `frozen`), and code that
//!   assumed fewer of them is right for a program whose callers guarantee more.
//!
//!   Equal link fingerprints do not prove two things. Callees: functions are named by symbol,
//!   so a callee whose body changed leaves its caller's fingerprint unchanged. A function may
//!   link only when every function it reaches matches too: direct calls, `Const::Func` in its
//!   operands (closures, glue pointers) and in relocations of the statics it reaches, internal
//!   functions included (closures, glue, generic instances, and small functions the object
//!   build inlined into its copy). And statics compared or written by address: statics are
//!   equal by contents, but the object's copy of a static is another address than the
//!   program's, so a static atomic (`StaticData::mutable`, such as the words of
//!   std/sync/epoch.vlt) or a table compared by address would be two words or two tables
//!   ([`Canon::reaches_mutable_static`]; std-object.md "Identity of statics").
//!
//! Fingerprints are std's SipHash with fixed keys: the same for one compiler binary, which is all
//! either use needs (a session; a std object is keyed by the compiler that built it).

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use crate::vir::{
    AggId, Callee, Const, Function, Operand, Place, Program, Proj, Rvalue, StaticId, Stmt,
    Terminator, Ty,
};

/// A 128-bit fingerprint (two independent 64-bit hashes of the canonical text).
pub type Fingerprint = u128;

/// What the fingerprints are for (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Versions of one program in a `velt dev` session.
    HotReload,
    /// Code compiled into another object (the cached std object), linked into this program.
    Link,
}

/// Is `name` the aggregate of an async state machine (pinned to its version in hot reload)?
pub fn is_state_layout(name: &str) -> bool {
    name.ends_with(" state") || name.ends_with(" promise")
}

/// Canonical spellings of one program's aggregates and statics, computed once.
pub struct Canon<'p> {
    program: &'p Program,
    mode: Mode,
    aggs: Vec<String>,
    statics: Vec<Fingerprint>,
}

impl<'p> Canon<'p> {
    /// Precompute the program's aggregate and static spellings.
    pub fn new(program: &'p Program, mode: Mode) -> Self {
        let mut canon = Canon {
            program,
            mode,
            aggs: vec![],
            statics: vec![],
        };
        let mut aggs = vec![None; program.aggs.len()];
        for i in 0..program.aggs.len() {
            canon.agg_text(AggId(i as u32), 0, &mut aggs);
        }
        canon.aggs = aggs.into_iter().map(Option::unwrap_or_default).collect();
        canon.statics = (0..program.statics.len())
            .map(|i| canon.static_print(StaticId(i as u32), &mut vec![]))
            .collect();
        canon
    }

    /// The structural spelling of aggregate `id` (hot reload: state machines by name only).
    pub fn agg(&self, id: AggId) -> &str {
        self.aggs.get(id.0 as usize).map_or("?", String::as_str)
    }

    /// The fingerprint of `func`: signature, locals and code.
    pub fn function(&self, func: &Function) -> Fingerprint {
        fingerprint(&self.text(func))
    }

    /// The canonical text [`Self::function`] fingerprints, for a key of its own (the std cache
    /// hashes it with SHA-256 next to the compiler and target).
    pub fn text(&self, func: &Function) -> String {
        let mut out = String::new();
        let _ = write!(out, "fn(");
        func.params.iter().for_each(|t| self.ty(&mut out, *t));
        let _ = write!(out, ")");
        self.ty(&mut out, func.ret);
        if self.mode == Mode::Link {
            let _ = write!(out, " poll={} target={:?}", func.is_poll, func.target);
        }
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
        out
    }

    /// The spelling of aggregate `id`. Hot reload: computed per path (state machines and levels
    /// past 16 by name). Link: whole, once per aggregate (`memo`; aggregates nest by value, never
    /// in a cycle).
    fn agg_text(&self, id: AggId, depth: u32, memo: &mut Vec<Option<String>>) -> String {
        let link = self.mode == Mode::Link;
        if let Some(Some(done)) = memo.get(id.0 as usize).filter(|_| link || depth == 0) {
            return done.clone();
        }
        let Some(layout) = self.program.aggs.get(id.0 as usize) else {
            return format!("agg?{}", id.0);
        };
        if !link && (is_state_layout(&layout.name) || depth > 16) {
            return format!("{{{}}}", layout.name);
        }
        let mut out = format!("{{{} {}/{}", layout.name, layout.size, layout.align);
        for (ty, offset) in &layout.fields {
            match ty {
                Ty::Agg(inner) => {
                    let inner = self.agg_text(*inner, depth + 1, memo);
                    let _ = write!(out, " {inner}@{offset}");
                }
                scalar => {
                    let _ = write!(out, " {scalar:?}@{offset}");
                }
            }
        }
        out.push('}');
        if link || depth == 0 {
            if let Some(slot) = memo.get_mut(id.0 as usize) {
                *slot = Some(out.clone());
            }
        }
        out
    }

    /// The fingerprint of static `id`'s contents and of what its relocations point at. Hot
    /// reload: statics 8 levels down count by contents only. Link: the whole graph; a static
    /// already on the way to this one (`path`, a cycle such as two vtables naming each other)
    /// counts by its position on the way.
    fn static_print(&self, id: StaticId, path: &mut Vec<StaticId>) -> Fingerprint {
        let Some(data) = self.program.statics.get(id.0 as usize) else {
            return 0;
        };
        let depth = path.len();
        path.push(id);
        let mut out = format!("{} {:?}", data.align, data.bytes);
        // Static atomics with the same starting value are different words.
        if let Some(id) = data.mutable {
            let _ = write!(out, " M{id:x}");
        }
        for (offset, target) in &data.relocs {
            let _ = write!(out, " @{offset}=");
            let on_path = path.iter().position(|p| p == target_static(target));
            match target {
                Const::Static(_) if self.mode == Mode::Link && on_path.is_some() => {
                    let _ = write!(out, "back{}", on_path.unwrap_or_default());
                }
                // Hot reload: static tables point at each other only shallowly (vtables, string
                // objects).
                Const::Static(s) if self.mode == Mode::Link || depth < 8 => {
                    let _ = write!(out, "S{:x}", self.static_print(*s, path));
                }
                other => self.constant(&mut out, other),
            }
        }
        path.pop();
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
            Const::Extern(e) => {
                self.extern_ref(out, e.0);
                Ok(())
            }
        };
    }

    /// An extern by symbol; link mode adds its declaration.
    fn extern_ref(&self, out: &mut String, id: u32) {
        let _ = write!(out, " E{}", self.extern_name(id));
        if let Some(e) = self.program.externs.get(id as usize) {
            if self.mode == Mode::Link {
                let _ = write!(out, "({:?}){:?}{}", e.params, e.ret, e.noreturn);
            }
        }
    }

    /// Whether `func` reaches a static the program writes (`StaticData::mutable`): through its
    /// operands, and through the relocations of the statics it reaches. Its copy in another
    /// object would write another word (module docs). It looks only at `func`'s own code and
    /// statics, not its callees', so it must run on every function of the link fixpoint.
    pub fn reaches_mutable_static(&self, func: &Function) -> bool {
        let mut seen = vec![false; self.program.statics.len()];
        let mut work: Vec<StaticId> = vec![];
        let mut push = |c: &Const, work: &mut Vec<StaticId>| {
            if let Const::Static(s) = c {
                if let Some(slot) = seen.get_mut(s.0 as usize).filter(|done| !**done) {
                    *slot = true;
                    work.push(*s);
                }
            }
        };
        for op in operands(func) {
            if let Operand::Const(c, _) = op {
                push(c, &mut work);
            }
        }
        while let Some(id) = work.pop() {
            let Some(data) = self.program.statics.get(id.0 as usize) else {
                continue;
            };
            if data.mutable.is_some() {
                return true;
            }
            data.relocs.iter().for_each(|(_, c)| push(c, &mut work));
        }
        false
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
                self.extern_ref(out, e.0);
                if self.extern_name(e.0) == "velt_rt_set_throw_loc" && self.mode == Mode::HotReload
                {
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

/// Whether code compiled for `compiled`'s parameter attributes is right for callers that uphold
/// `here`'s (vir.rs invariant 9): every fact it assumed holds here too. Assuming fewer is fine.
pub fn attrs_hold(compiled: &Function, here: &Function) -> bool {
    compiled.params.len() == here.params.len()
        && (0..compiled.params.len()).all(|i| {
            let (a, b) = (compiled.param_attr(i), here.param_attr(i));
            (!a.noalias || b.noalias)
                && (!a.readonly || b.readonly)
                && (!a.frozen || b.frozen)
                && (!a.nonnull || b.nonnull)
                && a.dereferenceable <= b.dereferenceable
        })
}

/// Every operand of `func`'s code.
fn operands(func: &Function) -> Vec<&Operand> {
    let mut out = vec![];
    for block in &func.blocks {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => out.push(a),
                    Rvalue::Binary(_, a, b) => out.extend([a, b]),
                    Rvalue::Aggregate(_, ops) => out.extend(ops),
                    Rvalue::AddrOf(_) => {}
                },
                Stmt::MemCopy { dst, src, .. } => out.extend([dst, src]),
                Stmt::MemCopyDyn { dst, src, len, .. } => out.extend([dst, src, len]),
                Stmt::MemSet { dst, byte, len } => out.extend([dst, byte, len]),
                Stmt::Nop => {}
            }
        }
        match &block.term {
            Terminator::Branch { cond: a, .. }
            | Terminator::Switch { value: a, .. }
            | Terminator::Return(a) => out.push(a),
            Terminator::Call { callee, args, .. } => {
                if let Callee::Ptr { target, .. } = callee {
                    out.push(target);
                }
                out.extend(args);
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
    out
}

/// The static a relocation target names, if it is one (else an id no static has).
fn target_static(target: &Const) -> &StaticId {
    const NONE: StaticId = StaticId(u32::MAX);
    match target {
        Const::Static(s) => s,
        _ => &NONE,
    }
}

/// Two independent 64-bit hashes of `text` (std's SipHash with fixed keys, so the result is the
/// same for one compiler binary).
pub fn fingerprint(text: &str) -> Fingerprint {
    let mut a = DefaultHasher::new();
    text.hash(&mut a);
    let mut b = DefaultHasher::new();
    0x5eed_u32.hash(&mut b);
    text.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}
