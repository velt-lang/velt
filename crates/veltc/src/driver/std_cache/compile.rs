//! Compiling a std function on its own, and storing its object.
//!
//! The function is compiled in a program of its own: itself (exported), the functions it may
//! inline (`select::Depth`; they are in its key: declarations keeping their bodies in a debug
//! build's object, internal copies of its own in a release build's), declarations of what
//! those call, copies of the plain statics they name, and the statics with identity they reach
//! as `Import`s under the program's names for them, with their contents (`statics`), and
//! declarations of the functions those name; the aggregates and externs are the program's, so
//! their ids stay. It is optimized with
//! unknown callers (velt_opt treats a declaration as opaque, an exported function as called from
//! anywhere) and compiled by LLVM to an object of its own. The program's entry is declared,
//! never defined, in such an object (vir.rs invariant 5).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use velt_build_cache::{Cache, Key};
use velt_vir::vir::{
    self, Callee, Const, FuncId, Linkage, Operand, Rvalue, StaticId, Stmt, Terminator,
};

use super::statics::{self, Names};
use super::{key, select, KIND};

/// Compile each of `programs` (a function's program of its own, [`program_of`]) in parallel:
/// its object, or why it could not be.
pub(super) fn each(
    programs: Vec<vir::Program>,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Vec<Result<Vec<u8>, String>> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![Err(String::new()); programs.len()]);
    // Each worker takes the next program out (moved, not cloned).
    let programs: Vec<Mutex<Option<vir::Program>>> =
        programs.into_iter().map(|p| Mutex::new(Some(p))).collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(programs.len()) {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, Ordering::Relaxed);
                let Some(slot) = programs.get(n) else {
                    return;
                };
                let program = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
                let Some(program) = program else {
                    continue;
                };
                let object = alone(program, cg);
                results.lock().unwrap_or_else(PoisonError::into_inner)[n] = object;
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// The program of function `i` alone (module docs); `names`: the program's statics' names. Every
/// function in it, defined or declared, has the parameter attributes `assumed` keeps of the
/// program's, as its key holds them.
pub(super) fn program_of(
    program: &vir::Program,
    i: usize,
    names: &Names,
    depth: select::Depth,
    assumed: key::Assumed,
) -> vir::Program {
    let bodies = select::inlinable(program, i, depth);
    let in_bodies: HashSet<usize> = bodies.iter().copied().collect();
    let reached = statics::reached(program, &bodies);
    // At `Depth::Whole`, the object's own copies of what it is compiled with, as the program's
    // are internal to it: velt_opt and LLVM then optimize them with what calls them, as in the
    // program (as declarations keeping their bodies, `floatText` stayed a call in `f64Text`'s
    // object, which ran 5% slower). Not those whose address is taken: the program's address
    // must stay the one address.
    let own_copy = own_copies(program, &bodies, &reached, depth);
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
    for c in statics::functions(program, &reached) {
        add(c, &mut order);
    }
    let mut alone = vir::Program {
        aggs: program.aggs.clone(),
        externs: program.externs.clone(),
        ..vir::Program::default()
    };
    // The statics with identity, as the program's: named, defined there.
    let mut statics: HashMap<u32, StaticId> = (reached.iter().enumerate())
        .map(|(n, &s)| (s as u32, StaticId(n as u32)))
        .collect();
    for &s in &reached {
        let mut data = program.statics[s].clone();
        data.symbol = names.of(s).map(str::to_string);
        data.linkage = Linkage::Import;
        for (_, target) in &mut data.relocs {
            match target {
                Const::Func(f) => *f = funcs[&(f.0 as usize)],
                Const::Static(t) => *t = statics[&t.0],
                _ => {}
            }
        }
        alone.statics.push(data);
    }
    for &j in &order {
        let mut f = program.funcs[j].clone();
        f.param_attrs = assumed.attrs(&f.param_attrs);
        f.locs.clear();
        // No line info, so no variables: this program has no files or types for them.
        f.locals.iter_mut().for_each(|l| l.debug = None);
        f.linkage = if j == i {
            Linkage::Export
        } else if own_copy.contains(&j) {
            Linkage::Internal
        } else {
            Linkage::Import
        };
        if in_bodies.contains(&j) {
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

/// Of `bodies` (function `bodies[0]` and what it is compiled with at `depth`, which reach the
/// statics with identity `reached`), those its object has copies of its own of, internal to it:
/// at `Depth::Whole`, all but itself and those whose address is taken. The program's address
/// must stay the one address; and the others are declarations keeping their bodies, the
/// program's own, which it exports.
pub(super) fn own_copies(
    program: &vir::Program,
    bodies: &[usize],
    reached: &[usize],
    depth: select::Depth,
) -> HashSet<usize> {
    if depth == select::Depth::Small {
        return HashSet::new();
    }
    let mut taken: HashSet<usize> = statics::functions(program, reached).into_iter().collect();
    for &j in bodies {
        select::visit(&program.funcs[j], &mut |op| {
            if let Operand::Const(Const::Func(f), _) = op {
                taken.insert(f.0 as usize);
            }
        });
    }
    (bodies.iter().skip(1).copied())
        .filter(|j| !taken.contains(j))
        .collect()
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
                    Rvalue::Aggregate(_, ops) | Rvalue::Intrinsic(_, ops) => {
                        ops.iter_mut().for_each(&mut op)
                    }
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

/// A function's program of its own, optimized with unknown callers and compiled to an object.
fn alone(mut alone: vir::Program, cg: &velt_codegen_cl::CodegenOptions) -> Result<Vec<u8>, String> {
    velt_opt::optimize(&mut alone, velt_opt::OptLevel::Speed);
    // The entry stays declared (`dead_funcs` drops a declaration nothing calls).
    if !alone.funcs.iter().any(|f| f.symbol == "velt_main") {
        let main = vir::Function::new("velt_main".into(), vec![], vir::Ty::I32, Linkage::Import);
        alone.funcs.push(main);
    }
    velt_vir::verify(&alone).map_err(|errs| errs.join("; "))?;
    // Not through the unit cache: the object is stored once, as the `stdfn` entry.
    let mut steps = vec![];
    let (objects, _) =
        velt_codegen_llvm::emit_objects_cached(&alone, cg, Some(1), &mut steps, None)?;
    objects.into_iter().next().ok_or_else(|| "no object".into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use velt_vir::vir::{
        AggId, AggLayout, BasicBlock, BinOp, BlockId, Local, LocalDecl, ParamAttrs, Place, Proj, Ty,
    };

    /// `(*p).0`, `p` pointing at a one-word cell.
    fn word(p: u32) -> Place {
        Place {
            local: Local(p),
            proj: vec![Proj::Deref(Ty::Agg(AggId(1))), Proj::Field(0)],
        }
    }

    fn copy(place: Place) -> Operand {
        Operand::Copy(place)
    }

    fn int(n: i128) -> Operand {
        Operand::Const(Const::Int(n), Ty::I64)
    }

    /// `count(p, q)` (std's, `@noInline`): `i = 0; while (i < (*p).0) { (*q).0 -= 1; i += 1 }`,
    /// returning `i`, with `p` `frozen readonly` as `attrs` say. `main(n)` returns
    /// `count(&cell, &cell)` for a cell holding `n`: with `p` and `q` the same, `n / 2` rounded up,
    /// and `n` where `(*p).0` is read once.
    fn program(frozen: bool) -> vir::Program {
        let local = |n: u32| Place::local(Local(n));
        let mut count = vir::Function::new(
            "_V3stdN5count".into(),
            vec![Ty::Ptr, Ty::Ptr],
            Ty::I64,
            Linkage::Internal,
        );
        count.inline = vir::Inline::Never;
        let cell = ParamAttrs {
            nonnull: true,
            dereferenceable: 8,
            ..ParamAttrs::default()
        };
        count.param_attrs = vec![
            ParamAttrs {
                frozen,
                readonly: frozen,
                ..cell
            },
            cell,
        ];
        count.locals.push(LocalDecl::new(Ty::I64, None)); // 2: i
        count.locals.push(LocalDecl::new(Ty::Bool, None)); // 3: i < (*p).0
        count.blocks = vec![
            BasicBlock {
                stmts: vec![Stmt::Assign(local(2), Rvalue::Use(int(0)))],
                term: Terminator::Goto(BlockId(1)),
            },
            BasicBlock {
                stmts: vec![Stmt::Assign(
                    local(3),
                    Rvalue::Binary(BinOp::Lt, copy(local(2)), copy(word(0))),
                )],
                term: Terminator::Branch {
                    cond: copy(local(3)),
                    then: BlockId(2),
                    els: BlockId(3),
                },
            },
            BasicBlock {
                stmts: vec![
                    Stmt::Assign(word(1), Rvalue::Binary(BinOp::Sub, copy(word(1)), int(1))),
                    Stmt::Assign(local(2), Rvalue::Binary(BinOp::Add, copy(local(2)), int(1))),
                ],
                term: Terminator::Goto(BlockId(1)),
            },
            BasicBlock {
                stmts: vec![],
                term: Terminator::Return(copy(local(2))),
            },
        ];
        let string = AggLayout {
            name: "string".into(),
            size: 24,
            align: 8,
            fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
        };
        let one_word = AggLayout {
            name: "cell".into(),
            size: 8,
            align: 8,
            fields: vec![(Ty::I64, 0)],
        };
        vir::Program {
            aggs: vec![string, one_word],
            funcs: vec![count],
            ..vir::Program::default()
        }
    }

    /// `main(n)` added to `p`, calling its `count` with the same cell twice.
    fn with_main(mut p: vir::Program) -> vir::Program {
        let count = p
            .funcs
            .iter()
            .position(|f| f.symbol == "_V3stdN5count")
            .unwrap();
        let mut main = vir::Function::new("main".into(), vec![Ty::I64], Ty::I64, Linkage::Export);
        main.locals.push(LocalDecl::new(Ty::Agg(AggId(1)), None)); // 1: the cell
        main.locals.push(LocalDecl::new(Ty::Ptr, None)); // 2: &cell
        main.locals.push(LocalDecl::new(Ty::I64, None)); // 3: the result
        let cell = Place {
            local: Local(1),
            proj: vec![Proj::Field(0)],
        };
        main.blocks = vec![
            BasicBlock {
                stmts: vec![
                    Stmt::Assign(cell, Rvalue::Use(copy(Place::local(Local(0))))),
                    Stmt::Assign(
                        Place::local(Local(2)),
                        Rvalue::AddrOf(Place::local(Local(1))),
                    ),
                ],
                term: Terminator::Call {
                    callee: Callee::Func(FuncId(count as u32)),
                    args: vec![copy(Place::local(Local(2))), copy(Place::local(Local(2)))],
                    dest: Some(Place::local(Local(3))),
                    next: BlockId(1),
                    unwind: None,
                },
            },
            BasicBlock {
                stmts: vec![],
                term: Terminator::Return(copy(Place::local(Local(3)))),
            },
        ];
        p.funcs.push(main);
        p
    }

    fn run(p: &vir::Program, n: u64) -> u64 {
        let host = velt_opt::interp::RecordingHost::default();
        let mut interp = velt_opt::interp::Interp::new(p, host);
        interp.call_symbol("main", &[n]).expect("runs")
    }

    /// `count`'s object compiled in a program where its `p` is `frozen`, optimized and run with
    /// `main`'s same cell twice (a program where `p` is not frozen).
    fn object(p: &vir::Program, assumed: key::Assumed) -> vir::Program {
        let canon = velt_vir::fingerprint::Canon::new(p, velt_vir::fingerprint::Mode::Link);
        let names = Names::new(p, &canon);
        let mut alone = with_main(program_of(p, 0, &names, select::Depth::Whole, assumed));
        velt_opt::optimize(&mut alone, velt_opt::OptLevel::Speed);
        alone
    }

    /// An object compiled with the program's `frozen` miscompiles where the param isn't: the loop
    /// reads `(*p).0` once. Compiled under `Assumed::Conservative`, as a release build stores its
    /// object for other programs to find, it computes what the code says in any program.
    #[test]
    fn an_object_without_whole_program_attributes_is_right_where_they_do_not_hold() {
        assert_eq!(run(&with_main(program(false)), 10), 5, "what the code says");
        let frozen = program(true);
        assert_eq!(
            run(&object(&frozen, key::Assumed::Program), 10),
            10,
            "miscompiled"
        );
        let conservative = object(&frozen, key::Assumed::Conservative);
        assert_eq!(run(&conservative, 10), 5);
        let count = &conservative.funcs[0];
        assert!(count
            .param_attrs
            .iter()
            .all(|a| !a.frozen && !a.readonly && !a.noalias));
        assert!(count
            .param_attrs
            .iter()
            .all(|a| a.nonnull && a.dereferenceable == 8));
    }
}
