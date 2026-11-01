//! Which std functions the cache serves (hand-built VIR).

use super::*;
use velt_vir::vir::{
    BasicBlock, FuncId, Function, Local, Place, Program, StaticData, StaticId, Ty,
};

/// A std function `name` of `stmts` statements, then a call of each of `calls` (by index), and
/// `extra` as its last statement.
fn func(name: &str, stmts: usize, calls: &[u32], extra: Option<Rvalue>) -> Function {
    let symbol = velt_vir::mangle::mangle(&format!("std/m::{name}"));
    let mut f = Function::new(symbol, vec![], Ty::I64, Linkage::Internal);
    f.locals.push(vir::LocalDecl::new(Ty::I64, None));
    f.locals.push(vir::LocalDecl::new(Ty::Ptr, None));
    let set = |v: i128| {
        Stmt::Assign(
            Place::local(Local(0)),
            Rvalue::Use(Operand::Const(Const::Int(v), Ty::I64)),
        )
    };
    let mut stmts: Vec<Stmt> = (0..stmts as i128).map(set).collect();
    if let Some(rv) = extra {
        stmts.push(Stmt::Assign(Place::local(Local(1)), rv));
    }
    let mut blocks = vec![];
    for (n, &c) in calls.iter().enumerate() {
        blocks.push(BasicBlock {
            stmts: std::mem::take(&mut stmts),
            term: Terminator::Call {
                callee: Callee::Func(FuncId(c)),
                args: vec![],
                dest: Some(Place::local(Local(0))),
                next: vir::BlockId(n as u32 + 1),
            },
        });
    }
    blocks.push(BasicBlock {
        stmts,
        term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
    });
    f.blocks = blocks;
    f
}

fn program(funcs: Vec<Function>, statics: Vec<StaticData>) -> Program {
    Program {
        funcs,
        statics,
        ..Program::default()
    }
}

fn stat(relocs: bool) -> StaticData {
    StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: if relocs {
            vec![(0, Const::Func(FuncId(0)))]
        } else {
            vec![]
        },
        mutable: None,
    }
}

fn addr(s: u32) -> Option<Rvalue> {
    Some(Rvalue::Use(Operand::Const(
        Const::Static(StaticId(s)),
        Ty::Ptr,
    )))
}

#[test]
fn large_std_functions_are_served_small_ones_stay() {
    // `keys::disambiguate` appends the suffix to the mangled symbol.
    let mut dup = func("big", 50, &[], None);
    dup.symbol.push_str("$dup2");
    let p = program(
        vec![
            func("big", 50, &[1], None),
            func("small", 3, &[], None),
            dup,
        ],
        vec![],
    );
    assert_eq!(candidates(&p), [0]);
    assert_eq!(
        inlinable(&p, 0),
        [0, 1],
        "the small callee is compiled into it"
    );
    let by = reasons(&p);
    assert_eq!((by["small"], by["$dup"]), (4, 51));
}

/// A static with relocations (a vtable, a type descriptor) or one the program writes keeps the
/// function in the program, also when only a small function it may inline names it.
#[test]
fn statics_with_identity_keep_a_function_in_the_program() {
    let p = program(
        vec![
            func("plain", 50, &[], addr(0)),
            func("table", 50, &[], addr(1)),
            func("through_small", 50, &[3], None),
            func("small", 3, &[], addr(1)),
        ],
        vec![stat(false), stat(true)],
    );
    assert_eq!(candidates(&p), [0]);
    let mut written = p.clone();
    written.statics[0].mutable = Some(Default::default());
    assert!(candidates(&written).is_empty());
}

/// A function that calls through a pointer (a callback) stays where `const_fields` can
/// specialize it for its caller's closure.
#[test]
fn callback_callers_stay_in_the_program() {
    let mut f = func("each", 50, &[], None);
    f.blocks[0].term = Terminator::Call {
        callee: Callee::Ptr {
            target: Operand::Copy(Place::local(Local(1))),
            params: vec![],
            ret: Ty::I64,
        },
        args: vec![],
        dest: Some(Place::local(Local(0))),
        next: vir::BlockId(1),
    };
    f.blocks.push(BasicBlock {
        stmts: vec![],
        term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
    });
    let p = program(vec![f], vec![]);
    assert!(candidates(&p).is_empty());
    assert!(reasons(&p).contains_key("callbacks"));
}
