//! What `velt dev` remembers about a program version to compare the next one with: per function
//! key its signature, fingerprint and the pinned functions it refers to; the layouts live data
//! may have; which keys live data may hold as code addresses; `main`'s keys; and what the
//! module state looks like.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    AggId, Callee, Const, Function, Operand, Place, Program, Proj, Rvalue, StaticId, Stmt,
    Terminator, Ty,
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
    /// Keys of the functions that reach the block (pass the descriptor to the runtime: a slot's
    /// address, or the whole block's).
    pub users: Vec<String>,
    /// The block's layout (the state tuple's structural spelling) and its slot count, read from
    /// the descriptor's init or drop function; `None` if neither shows it.
    pub layout: Option<(String, usize)>,
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
    // `velt_rt_module_state(desc, slot)` for a slot, `velt_rt_module_block(desc)` for the block.
    let reaches = |e: usize| {
        (program.externs.get(e)).is_some_and(|e| {
            e.symbol == "velt_rt_module_state" || e.symbol == "velt_rt_module_block"
        })
    };
    let mut desc = None;
    let mut users = vec![];
    for func in &program.funcs {
        let passed = func.blocks.iter().find_map(|b| match &b.term {
            Terminator::Call {
                callee: Callee::Extern(e),
                args,
                ..
            } if reaches(e.0 as usize) => match args.first() {
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
    // The descriptor's `init` (its word at offset 16) and `rest` (offset 40, the root file's
    // slots) write the block as the state tuple; its `drop` (offset 24) reads it so when a slot
    // has something to drop.
    let at = |offset: u32| {
        (data.relocs.iter()).find_map(|(at, t)| match t {
            Const::Func(f) if *at == offset => program.funcs.get(f.0 as usize),
            _ => None,
        })
    };
    let state_agg = [16, 40, 24]
        .into_iter()
        .find_map(|o| at(o).and_then(deref_agg));
    let layout = state_agg.map(|a| {
        let fields = program.aggs.get(a.0 as usize).map_or(0, |l| l.fields.len());
        (canon.agg(a).to_string(), fields)
    });
    Some(ModuleState {
        desc,
        key,
        users,
        layout,
    })
}

/// The aggregate `func` reads or writes its first parameter (the block) as.
fn deref_agg(func: &Function) -> Option<AggId> {
    let of = |p: &Place| match p.proj.first() {
        Some(Proj::Deref(Ty::Agg(a))) if p.local.0 == 0 => Some(*a),
        _ => None,
    };
    let operand = |o: &Operand| match o {
        Operand::Copy(p) => of(p),
        Operand::Const(..) => None,
    };
    for block in &func.blocks {
        for stmt in &block.stmts {
            let found = match stmt {
                Stmt::Assign(lhs, rv) => of(lhs).or_else(|| match rv {
                    Rvalue::AddrOf(p) => of(p),
                    _ => stmt_operands(stmt).into_iter().find_map(operand),
                }),
                _ => stmt_operands(stmt).into_iter().find_map(operand),
            };
            if found.is_some() {
                return found;
            }
        }
        if let Terminator::Call { args, dest, .. } = &block.term {
            if let Some(a) = args
                .iter()
                .find_map(operand)
                .or_else(|| dest.as_ref().and_then(of))
            {
                return Some(a);
            }
        }
    }
    None
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
