//! What `velt dev` remembers about a program version to compare the next one with: per function
//! key its signature, fingerprint and the pinned functions it refers to; the layouts live data
//! may have; which keys live data may hold as code addresses; and `main`'s keys.

use std::collections::{HashMap, HashSet};

use velt_vir::vir::{Callee, Const, Function, Program, Rvalue, Stmt, Terminator, Ty};

use super::fingerprint::{is_state_layout, Canon, Fingerprint};
use super::roles;

/// Facts about one function of a version.
pub(crate) struct FunctionFacts {
    /// Parameter and return types (scalars only, VIR invariant 2).
    pub signature: (Vec<Ty>, Ty),
    pub fingerprint: Fingerprint,
    /// Keys of the pinned functions it calls or takes the address of.
    pub pinned_refs: Vec<String>,
}

/// Facts about a whole version.
pub(crate) struct Facts {
    /// By function key (symbol).
    pub funcs: HashMap<String, FunctionFacts>,
    /// Per layout name: its structural spellings, each with its field count (state machines
    /// excluded: pinned). Generic types have one spelling per instance.
    pub layouts: HashMap<String, HashMap<String, usize>>,
    /// Keys whose address is taken (function values, closures, vtable slots): live data may
    /// still call them after an edit.
    pub address_taken: HashSet<String>,
    /// `velt_main` and the user `main` functions it starts (already running, never re-run).
    pub main: HashSet<String>,
}

impl Facts {
    /// Gather the facts of `program`.
    pub(crate) fn of(program: &Program) -> Facts {
        let canon = Canon::new(program);
        let mut facts = Facts {
            funcs: HashMap::new(),
            layouts: HashMap::new(),
            address_taken: HashSet::new(),
            main: HashSet::new(),
        };
        for (i, layout) in program.aggs.iter().enumerate() {
            if !is_state_layout(&layout.name) {
                let text = canon.agg(velt_vir::vir::AggId(i as u32)).to_string();
                facts
                    .layouts
                    .entry(layout.name.clone())
                    .or_default()
                    .insert(text, layout.fields.len());
            }
        }
        for data in &program.statics {
            for (_, target) in &data.relocs {
                if let Const::Func(f) = target {
                    facts.address_taken.insert(symbol(program, f.0));
                }
            }
        }
        for func in &program.funcs {
            let refs = references(func);
            for &(f, address) in &refs {
                if address {
                    facts.address_taken.insert(symbol(program, f));
                }
            }
            let mut pinned_refs: Vec<String> = refs
                .iter()
                .map(|&(f, _)| symbol(program, f))
                .filter(|s| roles::is_pinned(s))
                .collect();
            pinned_refs.sort();
            pinned_refs.dedup();
            if func.symbol == "velt_main" {
                facts.main = main_keys(program, &refs);
            }
            let fn_facts = FunctionFacts {
                signature: (func.params.clone(), func.ret),
                fingerprint: canon.function(func),
                pinned_refs,
            };
            facts.funcs.insert(func.symbol.clone(), fn_facts);
        }
        facts
    }
}

fn symbol(program: &Program, f: u32) -> String {
    program
        .funcs
        .get(f as usize)
        .map_or_else(String::new, |f| f.symbol.clone())
}

/// `velt_main` plus the user functions it calls or starts (not glue).
fn main_keys(program: &Program, refs: &[(u32, bool)]) -> HashSet<String> {
    let mut keys: HashSet<String> = refs
        .iter()
        .map(|&(f, _)| symbol(program, f))
        .filter(|s| !s.starts_with("_G"))
        .collect();
    keys.insert("velt_main".to_string());
    keys
}

/// Functions `func` refers to: (VIR function index, whether as an address rather than a call).
fn references(func: &Function) -> Vec<(u32, bool)> {
    let mut out = vec![];
    let operand = |op: &velt_vir::vir::Operand, out: &mut Vec<(u32, bool)>| {
        if let velt_vir::vir::Operand::Const(Const::Func(f), _) = op {
            out.push((f.0, true));
        }
    };
    for block in &func.blocks {
        for stmt in &block.stmts {
            for op in stmt_operands(stmt) {
                operand(op, &mut out);
            }
        }
        match &block.term {
            Terminator::Call { callee, args, .. } => {
                match callee {
                    Callee::Func(f) => out.push((f.0, false)),
                    Callee::Ptr { target, .. } => operand(target, &mut out),
                    Callee::Extern(_) => {}
                }
                args.iter().for_each(|a| operand(a, &mut out));
            }
            Terminator::Branch { cond: op, .. }
            | Terminator::Switch { value: op, .. }
            | Terminator::Return(op) => operand(op, &mut out),
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
    out
}

/// The operands a statement reads.
fn stmt_operands(stmt: &Stmt) -> Vec<&velt_vir::vir::Operand> {
    match stmt {
        Stmt::Assign(_, rv) => match rv {
            Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => vec![a],
            Rvalue::Binary(_, a, b) => vec![a, b],
            Rvalue::Aggregate(_, ops) => ops.iter().collect(),
            Rvalue::AddrOf(_) => vec![],
        },
        Stmt::MemCopy { dst, src, .. } => vec![dst, src],
        Stmt::MemCopyDyn { dst, src, len, .. } => vec![dst, src, len],
        Stmt::MemSet { dst, byte, len } => vec![dst, byte, len],
        Stmt::Nop => vec![],
    }
}
