//! Compiling a std function on its own, and storing its object.
//!
//! The function is compiled in a program of its own: itself (exported), the small functions it
//! may inline (declarations keeping their bodies; they are in its key), declarations of what
//! those call, and the statics they name (plain bytes: `select` keeps out functions reaching any
//! other); the aggregates and externs are the program's, so their ids stay. It is optimized with
//! unknown callers (velt_opt treats a declaration as opaque, an exported function as called from
//! anywhere) and compiled by LLVM to an object of its own. The program's entry is declared,
//! never defined, in such an object (vir.rs invariant 5).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use velt_build_cache::{Cache, Key};
use velt_vir::vir::{
    self, Callee, Const, FuncId, Linkage, Operand, Rvalue, StaticId, Stmt, Terminator,
};

use super::{select, KIND};

/// Compile each function of `misses` on its own, in parallel: its object, or `None` when it
/// could not be (it then stays in the program).
pub(super) fn each(
    program: &vir::Program,
    misses: &[(usize, Key)],
    cg: &velt_codegen_cl::CodegenOptions,
) -> Vec<Option<Vec<u8>>> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![None; misses.len()]);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(misses.len()) {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(i, _)) = misses.get(n) else {
                    return;
                };
                let object = alone(program, i, cg);
                results.lock().unwrap_or_else(PoisonError::into_inner)[n] = object;
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// The program of function `i` alone (module docs).
fn program_of(program: &vir::Program, i: usize) -> vir::Program {
    let bodies = select::inlinable(program, i);
    let mut funcs: HashMap<usize, FuncId> = HashMap::new();
    let mut order: Vec<usize> = vec![];
    let mut add = |j: usize, order: &mut Vec<usize>| {
        let n = funcs.len() as u32;
        *funcs.entry(j).or_insert_with(|| {
            order.push(j);
            FuncId(n)
        })
    };
    for &j in &bodies {
        add(j, &mut order);
    }
    for &j in &bodies {
        for c in select::referenced(&program.funcs[j]) {
            add(c, &mut order);
        }
    }
    let mut statics: HashMap<u32, StaticId> = HashMap::new();
    let mut alone = vir::Program {
        aggs: program.aggs.clone(),
        externs: program.externs.clone(),
        ..vir::Program::default()
    };
    for &j in &order {
        let mut f = program.funcs[j].clone();
        f.locs.clear();
        f.linkage = if j == i {
            Linkage::Export
        } else {
            Linkage::Import
        };
        if bodies.contains(&j) {
            remap(
                &mut f,
                &funcs,
                &mut statics,
                &program.statics,
                &mut alone.statics,
            );
        } else {
            f.blocks.clear();
            f.locals.truncate(f.params.len());
        }
        alone.funcs.push(f);
    }
    alone
}

/// Rename `f`'s functions and statics into the program alone, copying each static it names.
fn remap(
    f: &mut vir::Function,
    funcs: &HashMap<usize, FuncId>,
    statics: &mut HashMap<u32, StaticId>,
    from: &[vir::StaticData],
    to: &mut Vec<vir::StaticData>,
) {
    let mut op = |op: &mut Operand| match op {
        Operand::Const(Const::Func(id), _) => *id = funcs[&(id.0 as usize)],
        Operand::Const(Const::Static(id), _) => {
            *id = *statics.entry(id.0).or_insert_with(|| {
                to.push(from[id.0 as usize].clone());
                StaticId(to.len() as u32 - 1)
            });
        }
        _ => {}
    };
    for b in &mut f.blocks {
        for s in &mut b.stmts {
            match s {
                Stmt::Assign(_, rv) => match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a) | Rvalue::Cast(a, _) => op(a),
                    Rvalue::Binary(_, a, c) => {
                        op(a);
                        op(c);
                    }
                    Rvalue::Aggregate(_, ops) => ops.iter_mut().for_each(&mut op),
                    Rvalue::AddrOf(_) => {}
                },
                Stmt::MemCopy { dst, src, .. } => {
                    op(dst);
                    op(src);
                }
                Stmt::MemCopyDyn { dst, src, len, .. } => {
                    op(dst);
                    op(src);
                    op(len);
                }
                Stmt::MemSet { dst, byte, len } => {
                    op(dst);
                    op(byte);
                    op(len);
                }
                Stmt::Nop => {}
            }
        }
        match &mut b.term {
            Terminator::Branch { cond, .. } => op(cond),
            Terminator::Switch { value, .. } => op(value),
            Terminator::Return(o) => op(o),
            Terminator::Call { callee, args, .. } => {
                match callee {
                    Callee::Func(id) => *id = funcs[&(id.0 as usize)],
                    Callee::Ptr { target, .. } => op(target),
                    Callee::Extern(_) => {}
                }
                args.iter_mut().for_each(&mut op);
            }
            Terminator::Goto(_) | Terminator::Unreachable => {}
        }
    }
}

/// Function `i` of `program` compiled on its own, with unknown callers.
fn alone(
    program: &vir::Program,
    i: usize,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Option<Vec<u8>> {
    let mut alone = program_of(program, i);
    velt_opt::optimize(&mut alone, velt_opt::OptLevel::Speed);
    // The entry stays declared (`dead_funcs` drops a declaration nothing calls).
    if !alone.funcs.iter().any(|f| f.symbol == "velt_main") {
        let main = vir::Function::new("velt_main".into(), vec![], vir::Ty::I32, Linkage::Import);
        alone.funcs.push(main);
    }
    let not_compiled = |why: String| {
        if super::stats() {
            eprintln!(
                "velt: std cache: {} stays in the program: {why}",
                program.funcs[i].symbol
            );
        }
    };
    if let Err(errs) = velt_vir::verify(&alone) {
        not_compiled(errs.join("; "));
        return None;
    }
    let mut steps = vec![];
    match crate::backend::Backend::Llvm.emit_objects(&alone, cg, Some(1), &mut steps) {
        Ok((objects, _)) => objects.into_iter().next(),
        Err(e) => {
            not_compiled(e);
            None
        }
    }
}

/// Store `object` under `key`: the path of the cached file, which the link reads. Two builds
/// storing one key at once both get the one valid entry (`Cache::put_file`).
pub(super) fn store(cache: &Cache, key: &Key, object: &[u8]) -> Option<PathBuf> {
    let name = format!("velt-stdfn-{}-{}.o", std::process::id(), key.hex());
    let tmp = std::env::temp_dir().join(name);
    std::fs::write(&tmp, object).ok()?;
    let stored = cache.put_file(KIND, key, &tmp);
    let _ = std::fs::remove_file(&tmp);
    stored.then(|| cache.path_of(KIND, key)).flatten()
}
