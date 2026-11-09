//! What `velt dev` remembers about a program version to compare the next one with: per function
//! key its signature, fingerprint and the pinned functions it refers to; the layouts live data
//! may have; which keys live data may hold as code addresses; `main`'s keys; and what the
//! module state looks like.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    Callee, Const, Function, Operand, Program, Rvalue, StaticId, Stmt, Terminator, Ty,
};

use super::roles;
use velt_vir::fingerprint::{is_state_layout, Canon, Fingerprint, Mode};

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
    pub funcs: FxHashMap<String, FunctionFacts>,
    /// Per layout name: its structural spellings, each with its field count (state machines
    /// excluded: pinned). Generic types have one spelling per instance.
    pub layouts: FxHashMap<String, FxHashMap<String, usize>>,
    /// Keys whose address is taken (function values, closures, vtable slots): live data may
    /// still call them after an edit.
    pub address_taken: FxHashSet<String>,
    /// `velt_main` and the user `main` functions it starts (already running, never re-run).
    pub main: FxHashSet<String>,
    /// The program's module state, if it has any.
    pub module_state: Option<ModuleState>,
}

/// The module state of a version (module-level `let`s, velt_vir lower/module_state.rs): one
/// block the runtime keys by the address of the program's descriptor (`ModuleDesc`).
pub(crate) struct ModuleState {
    /// The descriptor's index in the program's statics.
    pub desc: StaticId,
    /// The descriptor by contents (size, alignment, slot names, which functions initialize and
    /// drop the block) and the fingerprints of those functions, which spell the block's layout
    /// and the initializers: two versions with the same key can share one block.
    pub key: Vec<Fingerprint>,
    /// Keys of the functions that reach the block (pass the descriptor to the runtime).
    pub users: Vec<String>,
}

impl Facts {
    /// Gather the facts of `program`.
    pub(crate) fn of(program: &Program) -> Facts {
        let canon = Canon::new(program, Mode::HotReload);
        let mut facts = Facts {
            funcs: FxHashMap::default(),
            layouts: FxHashMap::default(),
            address_taken: FxHashSet::default(),
            main: FxHashSet::default(),
            module_state: None,
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
        facts.module_state = module_state(program, &canon);
        facts
    }
}

/// The module state of `program`: its descriptor is the static every function reaching the
/// block passes to `velt_rt_module_state`.
fn module_state(program: &Program, canon: &Canon) -> Option<ModuleState> {
    let state = (program.externs.iter()).position(|e| e.symbol == "velt_rt_module_state")?;
    let mut desc = None;
    let mut users = vec![];
    for func in &program.funcs {
        let passed = func.blocks.iter().find_map(|b| match &b.term {
            Terminator::Call {
                callee: Callee::Extern(e),
                args,
                ..
            } if e.0 as usize == state => match args.first() {
                Some(Operand::Const(Const::Static(s), _)) => Some(*s),
                _ => None,
            },
            _ => None,
        });
        if let Some(s) = passed {
            desc = Some(s);
            users.push(func.symbol.clone());
        }
    }
    let desc = desc?;
    let data = program.statics.get(desc.0 as usize)?;
    let mut key = vec![canon.static_fingerprint(desc)];
    for (_, target) in &data.relocs {
        if let Const::Func(f) = target {
            key.extend(program.funcs.get(f.0 as usize).map(|f| canon.function(f)));
        }
    }
    users.sort();
    Some(ModuleState { desc, key, users })
}

fn symbol(program: &Program, f: u32) -> String {
    program
        .funcs
        .get(f as usize)
        .map_or_else(String::new, |f| f.symbol.clone())
}

/// `velt_main` plus the user functions it calls or starts (not glue).
fn main_keys(program: &Program, refs: &[(u32, bool)]) -> FxHashSet<String> {
    let mut keys: FxHashSet<String> = refs
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
            Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => ops.iter().collect(),
            Rvalue::AddrOf(_) => vec![],
        },
        Stmt::MemCopy { dst, src, .. } => vec![dst, src],
        Stmt::MemCopyDyn { dst, src, len, .. } => vec![dst, src, len],
        Stmt::MemSet { dst, byte, len } => vec![dst, byte, len],
        Stmt::Nop => vec![],
    }
}
