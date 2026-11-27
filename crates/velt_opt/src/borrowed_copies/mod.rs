//! Read-only string copies borrow their source instead of sharing it (#1670).
//!
//! `const src = this.src;` gives the local its own reference: lowering shares the string
//! (`str_share`: `velt_rt_str_clone` for a heap string, a plain copy of the three words for an
//! inline or static one) and drops it where the local dies (`str_drop`: `velt_rt_str_drop`, or
//! zeroing). Both runtime calls are atomic count updates. When the local is only read and
//! nothing can change the source while the local's text is still used, the count changes are
//! not needed: the local can hold a plain copy of the source's words (a borrow), as call
//! arguments already do (https://veltlang.com/docs/reference/memory/ "calls borrow").
//!
//! The pass works on a *family*: the string locals a clone's value moves through (whole copies,
//! a rebuild from its words), the register locals derived from their data word or address (data
//! pointers), and the clones and plain copies that give the family its values. A family is
//! borrowed when:
//! - its values never leave it: its data words and addresses are only used to compute other
//!   family values and pointers, to read memory, and as arguments of runtime functions that
//!   only read (`effects` below) or of the family's own clones and drops; every write to a
//!   string of the family is one of those clones, a plain copy of a clone's source, a move from
//!   the family, or the zero a drop leaves;
//! - nothing that may change the source runs while the family's text may still be read (a
//!   strong-liveness walk back from the reads through the text, i.e. through a family pointer
//!   or a read-only call): no call other than read-only runtime functions, allocation, and
//!   clones and drops of strings in locals; no store through a pointer, unless the source is a
//!   field of a class object and the store writes another field of a class object (layouts of
//!   one class and its bases agree, and objects never overlap: the TBAA rules of
//!   velt_codegen_llvm's `tbaa.rs`); no copy or fill through a pointer; no write to a local
//!   whose address is taken when the source may be in a local.
//!
//! Then each clone becomes a plain copy of the source's words, and each drop becomes the zeroing
//! it would have done. The source keeps its own reference, and so the text alive, for as long as
//! the family reads it.
//!
//! The same analysis has a views mode (`views.rs`, run after this one): slices of the family
//! join it, its drops stay, and a clone that starts it gives the family a static view of a heap
//! ASCII source instead of a borrow.

#[cfg(test)]
mod tests;
#[cfg(test)]
mod views_tests;

pub(crate) mod counted;
mod live;
mod read_only;
mod views;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::math::effects;
use velt_vir::vir::{
    AggLayout, BinOp, Callee, ExternId, FuncId, Function, Local, Operand, Place, Program, Proj,
    Rvalue, Stmt, Terminator, Ty, UnOp, STR_AGG,
};

use crate::locals::Usage;
use crate::srclocs::push_stmt;
use crate::visit::{stmt_operands, term_operands};

/// `velt_rt_str_clone(src, out)`: a new reference to the string at `src` in `*out`.
const STR_CLONE: &str = "velt_rt_str_clone";
/// `velt_rt_str_drop(s)`: gives up the reference of the string at `s` and zeroes it.
const STR_DROP: &str = "velt_rt_str_drop";
/// `velt_rt_alloc(size, align)`: fresh memory, nothing else written.
const ALLOC: &str = "velt_rt_alloc";
/// `velt_rt_str_slice(s, start, end, out)`: a slice of the string at `s` in `*out` (`views.rs`).
const STR_SLICE: &str = "velt_rt_str_slice";

/// The runtime functions this pass knows, by id in one program.
pub(crate) struct Runtime {
    clone: Option<ExternId>,
    drop: Option<ExternId>,
    alloc: Option<ExternId>,
    slice: Option<ExternId>,
    read_only: FxHashSet<ExternId>,
    /// Velt functions that write nothing their callers can see (`read_only.rs`).
    read_only_fns: FxHashSet<FuncId>,
    /// The position translation's functions (std/lang/string/positions.velt, [`translates`]),
    /// `charCodeAt` of non-ASCII text among them (#2816).
    code_units: FxHashSet<FuncId>,
}

/// Is `symbol` (an instance of) a function of std/lang/string/positions.velt that returns a
/// position, a code unit or nothing: the translation `charCodeAt` of non-ASCII text calls in
/// place of the runtime's read (#2816)? They write only the translation's per-thread cache and
/// the string's breadcrumbs, which compiled code never reads, as that runtime function did
/// (`Effects::ReadCaching`), and return no address. Not `key1`, which returns a data word.
fn translates(symbol: &str) -> bool {
    let entry = velt_vir::mangle::mangle(velt_common::std_hook!(string_char_code_at));
    let item = velt_common::std_hook_name!(string_char_code_at);
    let Some(module) = entry.strip_suffix(&format!("N{}{item}", item.len())) else {
        return false;
    };
    let Some(rest) = symbol
        .strip_prefix(module)
        .and_then(|r| r.strip_prefix('N'))
    else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let Ok(n) = rest[..digits].parse::<usize>() else {
        return false;
    };
    let Some(name) = rest.get(digits..digits + n) else {
        return false;
    };
    let tail = &rest[digits + n..];
    name != "key1" && (tail.is_empty() || tail.starts_with("_L"))
}

impl Runtime {
    pub(crate) fn find(program: &Program) -> Runtime {
        let ext = |name: &str| {
            program
                .externs
                .iter()
                .position(|e| e.symbol == name)
                .map(|i| ExternId(i as u32))
        };
        // Runtime functions that write no memory compiled code reads (`velt_vir::math`).
        let read_only: FxHashSet<ExternId> = (program.externs.iter().enumerate())
            .filter(|(_, e)| effects(&e.symbol).writes_nothing_visible())
            .map(|(i, _)| ExternId(i as u32))
            .collect();
        let code_units: FxHashSet<FuncId> = (program.funcs.iter().enumerate())
            .filter(|(_, f)| translates(&f.symbol))
            .map(|(i, _)| FuncId(i as u32))
            .collect();
        let read_only_fns = read_only::read_only_functions(program, &read_only, &code_units);
        Runtime {
            clone: ext(STR_CLONE),
            drop: ext(STR_DROP),
            alloc: ext(ALLOC),
            slice: ext(STR_SLICE),
            read_only_fns,
            read_only,
            code_units,
        }
    }

    fn is(&self, callee: &Callee, id: Option<ExternId>) -> bool {
        matches!(callee, Callee::Extern(e) if Some(*e) == id)
    }

    fn reads_only(&self, callee: &Callee) -> bool {
        match callee {
            Callee::Extern(e) => self.read_only.contains(e),
            Callee::Func(f) => self.read_only_fns.contains(f),
            Callee::Ptr { .. } => false,
        }
    }

    /// Does a call of `callee` return no address of its arguments as an integer: a runtime
    /// function's result (counts, positions, code units, hashes), or a position or code unit of
    /// the translation (`code_units`)?
    fn returns_no_address(&self, callee: &Callee) -> bool {
        match callee {
            Callee::Extern(_) => true,
            Callee::Func(f) => self.code_units.contains(f),
            Callee::Ptr { .. } => false,
        }
    }
}

/// Borrow every family of `func` that allows it; returns whether anything changed.
pub(crate) fn run(aggs: &[AggLayout], rt: &Runtime, func: &mut Function) -> bool {
    run_mode(aggs, rt, func, false)
}

/// Make the families of `func` that allow it string views (`views.rs`); returns whether
/// anything changed.
/// A view only pays where a family slices, and every slice of a string that is not static calls
/// the runtime's slice: a function with no such call has nothing for this mode that the copy
/// mode, run before it, did not take.
pub(crate) fn run_views(aggs: &[AggLayout], rt: &Runtime, func: &mut Function) -> bool {
    let slices = |b: &velt_vir::vir::BasicBlock| matches!(&b.term, Terminator::Call { callee, .. } if rt.is(callee, rt.slice));
    rt.slice.is_some() && func.blocks.iter().any(slices) && run_mode(aggs, rt, func, true)
}

fn run_mode(aggs: &[AggLayout], rt: &Runtime, func: &mut Function, views: bool) -> bool {
    let clones = |b: &velt_vir::vir::BasicBlock| matches!(&b.term, Terminator::Call { callee, .. } if rt.is(callee, rt.clone));
    if !func.blocks.iter().any(clones) {
        return false;
    }
    let usage = Usage::of(func);
    let mut addrs: FxHashMap<Local, &Place> = FxHashMap::default();
    for s in func.blocks.iter().flat_map(|b| &b.stmts) {
        if let Stmt::Assign(d, Rvalue::AddrOf(a)) = s {
            if d.proj.is_empty() {
                addrs.entry(d.local).or_insert(a);
            }
        }
    }
    let local_addresses = live::local_address_registers(func, &usage);
    let cx = Cx {
        aggs,
        rt,
        func,
        usage: &usage,
        addrs: &addrs,
        views,
        local_addresses: &local_addresses,
    };
    let mut done: FxHashSet<Local> = FxHashSet::default();
    // Members of the families borrowed so far: a family that shares one with them is left as
    // it is (its analysis assumed the other's clones stay).
    let mut claimed: FxHashSet<Local> = FxHashSet::default();
    let mut edits: Vec<Edit> = vec![];
    for bi in 0..func.blocks.len() {
        let Some(m) = cx.clone_into(bi) else { continue };
        if done.contains(&m) {
            continue;
        }
        let family = cx.family(m);
        let overlaps = family.members.iter().any(|x| claimed.contains(x));
        done.extend(family.members.iter().copied());
        if let Some(found) = family.filter(|f| !overlaps && cx.borrowable(f)) {
            claimed.extend(found.members.iter().copied());
            edits.extend(found.edits);
        }
    }
    if edits.is_empty() {
        return false;
    }
    for e in edits {
        match e {
            Edit::Demote { block, m, src } => views::demote(func, block, m, src),
            e => apply(func, e),
        }
    }
    true
}

/// A change to one call of a borrowed family.
enum Edit {
    /// The clone in block `block` becomes `m = *src`.
    Copy { block: usize, m: Local, src: Local },
    /// The drop in block `block` becomes zeroing `m`.
    Zero { block: usize, m: Local },
    /// The clone in block `block` becomes a static view of `*src` when that is a heap ASCII
    /// string, and stays the clone otherwise (`views.rs`).
    Demote { block: usize, m: Local, src: Local },
}

fn apply(func: &mut Function, e: Edit) {
    let (bi, stmts) = match e {
        Edit::Copy { block, m, src } => {
            let from = Place {
                local: src,
                proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
            };
            let s = Stmt::Assign(Place::local(m), Rvalue::Use(Operand::Copy(from)));
            (block, vec![s])
        }
        Edit::Zero { block, m } => {
            let zero = |k| {
                let field = Place {
                    local: m,
                    proj: vec![Proj::Field(k)],
                };
                Stmt::Assign(
                    field,
                    Rvalue::Use(Operand::Const(velt_vir::vir::Const::Int(0), Ty::U64)),
                )
            };
            (block, (0..3).map(zero).collect())
        }
        Edit::Demote { .. } => panic!("ICE: borrowed_copies applies a demotion as a copy"),
    };
    let at = func.term_loc(bi);
    let Terminator::Call { next, .. } = func.blocks[bi].term else {
        panic!("ICE: borrowed_copies edits a call that is gone");
    };
    for s in stmts {
        push_stmt(func, bi, s, at);
    }
    func.blocks[bi].term = Terminator::Goto(next);
}

/// What a family is made of (see the module doc).
#[derive(Default)]
struct Family {
    /// String locals holding a family value.
    members: FxHashSet<Local>,
    /// Register locals holding a family data word or a pointer derived from one or from a
    /// member's address.
    pointers: FxHashSet<Local>,
    /// Pointers that are exactly a member's address (`p = &m`).
    addr_of: FxHashMap<Local, Local>,
    /// The locals holding the source pointers of the family's clones.
    sources: Vec<Local>,
    edits: Vec<Edit>,
    /// Something the pass cannot account for.
    bail: bool,
}

impl Family {
    fn filter(self, keep: impl FnOnce(&Family) -> bool) -> Option<Family> {
        (!self.bail && keep(&self)).then_some(self)
    }

    fn tracked(&self, l: Local) -> bool {
        self.members.contains(&l) || self.pointers.contains(&l)
    }
}

/// Where a clone's source string lives.
#[derive(Clone, Copy)]
enum Source {
    /// Bytes `start..start + 24` of a class object (`agg`).
    Field { start: u64 },
    /// A string the caller holds (views mode): reached through a parameter, so never one of this
    /// function's locals, but possibly anywhere else.
    Caller,
    /// Anywhere (a parameter, a local's address, ...).
    Unknown,
}

struct Cx<'a> {
    aggs: &'a [AggLayout],
    rt: &'a Runtime,
    func: &'a Function,
    usage: &'a Usage,
    /// The place each local's first `l = &place` takes the address of.
    addrs: &'a FxHashMap<Local, &'a Place>,
    /// Views mode (`views.rs`): members may also hold views and owned strings, slices of them
    /// join, drops stay, and the clones that start a family become demotions.
    views: bool,
    /// The registers that only ever hold the address of one of this function's locals
    /// (`live::local_address_registers`).
    local_addresses: &'a FxHashSet<Local>,
}

/// A program point: block, then statement index (`stmts.len()` = the terminator).
type Point = (usize, usize);

impl Cx<'_> {
    /// `velt_rt_str_clone(_, &m)` ending block `bi`: `m`.
    fn clone_into(&self, bi: usize) -> Option<Local> {
        let Terminator::Call { callee, args, .. } = &self.func.blocks[bi].term else {
            return None;
        };
        if !self.rt.is(callee, self.rt.clone) || args.len() != 2 {
            return None;
        }
        self.addr_of_local(&args[1])
            .filter(|m| self.is_string_local(*m))
    }

    fn is_string_local(&self, l: Local) -> bool {
        l.0 as usize >= self.func.params.len()
            && self.func.locals.get(l.0 as usize).map(|d| d.ty) == Some(Ty::Agg(STR_AGG))
    }

    /// The local `op` is the address of: `op` is a register written once, with `&l`.
    fn addr_of_local(&self, op: &Operand) -> Option<Local> {
        let Operand::Copy(p) = op else { return None };
        if !p.proj.is_empty() || !self.usage.is_register(p.local) {
            return None;
        }
        if self.usage.get(p.local).defs != 1 || (p.local.0 as usize) < self.func.params.len() {
            return None;
        }
        let a = self.addrs.get(&p.local)?;
        a.proj.is_empty().then_some(a.local)
    }

    /// The family of string local `m` (closed under the moves and derivations of the module
    /// doc), with `bail` set when some use of it is not understood.
    fn family(&self, m: Local) -> Family {
        let mut f = Family::default();
        f.members.insert(m);
        // Grow until nothing joins, then classify every use once more with the final sets.
        loop {
            let before = (f.members.len(), f.pointers.len());
            f.bail = false;
            f.edits.clear();
            f.sources.clear();
            for (bi, block) in self.func.blocks.iter().enumerate() {
                for s in &block.stmts {
                    self.stmt(&mut f, s);
                }
                self.term(&mut f, bi, &block.term);
            }
            if (f.members.len(), f.pointers.len()) == before || f.bail {
                break;
            }
        }
        if !f.bail {
            self.check_member_defs(&mut f);
        }
        f
    }

    /// Whether operand `op` carries a family value (data word or pointer); a whole member is
    /// a move, not a pointer, and handled by the caller.
    fn tainted(&self, f: &Family, op: &Operand) -> bool {
        let Operand::Copy(p) = op else { return false };
        match p.proj.first() {
            None => f.pointers.contains(&p.local),
            // A member's data word.
            Some(Proj::Field(0)) => f.members.contains(&p.local) && p.proj.len() == 1,
            Some(Proj::Field(_)) => false,
            Some(Proj::Deref(t)) => {
                if !f.pointers.contains(&p.local) {
                    return false;
                }
                // Read through a family pointer: a member's data word, or any part of one
                // wider than two bytes, may be one; text is read a byte at a time (`u8`, or a
                // field of `velt_vir::STR_TEXT_AGG_NAME`).
                match (t, &p.proj[1..]) {
                    (Ty::Agg(a), [Proj::Field(0)]) if *a == STR_AGG => true,
                    (Ty::Agg(a), [Proj::Field(_)]) if *a == STR_AGG => false,
                    _ => !crate::locals::place_ty(self.aggs, self.func, p).is_some_and(|t| {
                        matches!(t, Ty::U8 | Ty::I8 | Ty::U16 | Ty::I16 | Ty::Bool)
                    }),
                }
            }
            Some(Proj::Cast(_)) => f.members.contains(&p.local),
        }
    }

    /// Whether `op` reads a whole member (or a whole string through a family pointer).
    fn whole_member(&self, f: &Family, op: &Operand) -> bool {
        let Operand::Copy(p) = op else { return false };
        match &p.proj[..] {
            [] => f.members.contains(&p.local),
            [Proj::Deref(Ty::Agg(a))] => *a == STR_AGG && f.pointers.contains(&p.local),
            _ => false,
        }
    }

    /// Whether `pl` (a written place) names family memory: a member, or memory through a
    /// family pointer.
    fn writes_family(&self, f: &Family, pl: &Place) -> bool {
        f.members.contains(&pl.local) || (f.pointers.contains(&pl.local) && !pl.proj.is_empty())
    }

    fn stmt(&self, f: &mut Family, s: &Stmt) {
        match s {
            Stmt::Assign(dst, rv) => self.assign(f, dst, rv),
            Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => {
                let mut touches = false;
                stmt_operands(s, &mut |op| {
                    touches |= self.tainted(f, op) || self.whole_member(f, op);
                });
                f.bail |= touches;
            }
            Stmt::Nop => {}
        }
    }

    fn assign(&self, f: &mut Family, dst: &Place, rv: &Rvalue) {
        let mut tainted = false;
        let mut whole = false;
        crate::visit::rvalue_operands(rv, &mut |op| {
            tainted |= self.tainted(f, op);
            whole |= self.whole_member(f, op);
        });
        let addr = match rv {
            Rvalue::AddrOf(a) => f.members.contains(&a.local) || f.pointers.contains(&a.local),
            _ => false,
        };
        // Comparisons and conversions to `bool` produce no pointer.
        let flag = matches!(
            rv,
            Rvalue::Binary(
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
                ..
            ) | Rvalue::Cast(_, Ty::Bool)
                | Rvalue::Unary(UnOp::Not, _)
        );
        let carries = (tainted && !flag) || addr;
        if !self.views && dst.proj.is_empty() && f.pointers.contains(&dst.local) && !carries {
            // A family pointer that also holds other values: they could be another string's
            // words, which the family would then take for its own (and not drop). Views mode
            // keeps every drop, so tracking more than the family's values is only conservative.
            f.bail = true;
            return;
        }
        let into_family = self.writes_family(f, dst);
        if dst.proj.is_empty() && !into_family && !carries && !whole {
            return;
        }
        if !dst.proj.is_empty() {
            // A part of a place: only the zero a drop leaves in a member is understood.
            if into_family {
                let zero = matches!(rv, Rvalue::Use(Operand::Const(c, _)) if *c == velt_vir::vir::Const::Int(0));
                let member_field = match &dst.proj[..] {
                    [Proj::Field(k)] => *k < 3 && f.members.contains(&dst.local),
                    [Proj::Deref(Ty::Agg(a)), Proj::Field(k)] => {
                        *a == STR_AGG && *k < 3 && f.addr_of.contains_key(&dst.local)
                    }
                    _ => false,
                };
                f.bail |= !(zero && member_field);
            } else if carries || whole {
                // A family value stored into memory: it may outlive the borrow.
                f.bail = true;
            }
            return;
        }
        let x = dst.local;
        if whole || (matches!(rv, Rvalue::Aggregate(a, _) if *a == STR_AGG) && carries) {
            if self.is_string_local(x) {
                f.members.insert(x);
            } else {
                f.bail = true;
            }
            return;
        }
        if carries {
            if matches!(rv, Rvalue::Aggregate(..)) || !self.usage.is_register(x) {
                f.bail = true;
                return;
            }
            f.pointers.insert(x);
            if let Rvalue::AddrOf(a) = rv {
                if a.proj.is_empty() && f.members.contains(&a.local) && self.usage.get(x).defs == 1
                {
                    f.addr_of.insert(x, a.local);
                }
            }
        }
        // A whole member written from elsewhere is checked by `check_member_defs`.
    }

    fn term(&self, f: &mut Family, bi: usize, t: &Terminator) {
        match t {
            Terminator::Call {
                callee, args, dest, ..
            } => {
                if let Some(d) = dest {
                    // In views mode a call may define a whole member or a tracked register
                    // (`views.rs`: tracking more than the family's values is only conservative
                    // there); a write through a family pointer still lets nothing be borrowed.
                    let defines = self.views && d.proj.is_empty() && f.tracked(d.local);
                    let pointer = !self.views && f.pointers.contains(&d.local);
                    if (self.writes_family(f, d) && !defines) || pointer {
                        f.bail = true;
                    }
                }
                let marked: Vec<bool> = args
                    .iter()
                    .map(|a| self.tainted(f, a) || self.whole_member(f, a))
                    .collect();
                if let Callee::Ptr { target, .. } = callee {
                    f.bail |= self.tainted(f, target);
                }
                if self.rt.is(callee, self.rt.clone) && args.len() == 2 {
                    if self.views && marked[0] {
                        // A clone of a family value is a bitwise copy of a view: it joins.
                        self.join_out(f, &args[1]);
                        return;
                    }
                    if marked[1] {
                        // A clone into the family: its source must be a plain pointer local.
                        let into = self
                            .addr_of_local(&args[1])
                            .filter(|m| f.members.contains(m));
                        match (into, &args[0]) {
                            (Some(m), Operand::Copy(s))
                                if s.proj.is_empty() && !f.tracked(s.local) =>
                            {
                                f.sources.push(s.local);
                                let src = s.local;
                                f.edits.push(if self.views {
                                    Edit::Demote { block: bi, m, src }
                                } else {
                                    Edit::Copy { block: bi, m, src }
                                });
                            }
                            _ => f.bail = true,
                        }
                    }
                    // A clone *of* the family reads its text (and count): a use (`live.rs`).
                    return;
                }
                if self.rt.is(callee, self.rt.drop) && args.len() == 1 {
                    if marked[0] {
                        match self
                            .addr_of_local(&args[0])
                            .filter(|m| f.members.contains(m))
                        {
                            // A member's drop stays in views mode: it may hold an owned string.
                            Some(m) if !self.views => f.edits.push(Edit::Zero { block: bi, m }),
                            Some(_) => {}
                            None => f.bail = true,
                        }
                    }
                    return;
                }
                if self.views && self.rt.is(callee, self.rt.slice) && args.len() == 4 {
                    // A slice of a family value may be a view of the same bytes: it joins. Its
                    // positions are numbers, never family values.
                    if marked[0] {
                        self.join_out(f, &args[3]);
                    }
                    f.bail |= marked[1] || marked[2];
                    return;
                }
                if marked.contains(&true) && !self.rt.reads_only(callee) {
                    f.bail = true;
                }
                if self.views && marked.contains(&true) {
                    if let Some(d) = dest {
                        self.call_result(f, callee, d);
                    }
                }
            }
            t => term_operands(t, &mut |op| {
                // A branch on a family pointer reveals nothing; a return lets it out.
                if matches!(t, Terminator::Return(_))
                    && (self.tainted(f, op) || self.whole_member(f, op))
                {
                    f.bail = true;
                }
            }),
        }
    }

    /// Every whole write of a member is a clone (seen in `term`), a move from the family, or a
    /// plain copy of a clone's source.
    fn check_member_defs(&self, f: &mut Family) {
        if self.views {
            // Members may hold any string (their drops stay): a demotion is all that is needed.
            f.bail |= !f.edits.iter().any(|e| matches!(e, Edit::Demote { .. }));
            return;
        }
        for block in &self.func.blocks {
            for s in &block.stmts {
                let Stmt::Assign(dst, rv) = s else { continue };
                if !dst.proj.is_empty() || !f.members.contains(&dst.local) {
                    continue;
                }
                let ok = match rv {
                    Rvalue::Use(op) if self.whole_member(f, op) => true,
                    Rvalue::Aggregate(a, ops) => {
                        *a == STR_AGG && ops.first().is_some_and(|op| self.tainted(f, op))
                    }
                    Rvalue::Use(Operand::Copy(p)) => {
                        p.proj == [Proj::Deref(Ty::Agg(STR_AGG))] && f.sources.contains(&p.local)
                    }
                    _ => false,
                };
                if !ok {
                    f.bail = true;
                    return;
                }
            }
        }
        if !f.edits.iter().any(|e| matches!(e, Edit::Copy { .. })) {
            f.bail = true;
        }
    }

    /// Views mode: the string written through out-pointer `op` joins the family; an out-pointer
    /// that is not a string local's address lets a view out.
    fn join_out(&self, f: &mut Family, op: &Operand) {
        match self.addr_of_local(op).filter(|m| self.is_string_local(*m)) {
            Some(m) => {
                f.members.insert(m);
            }
            None => f.bail = true,
        }
    }
}
