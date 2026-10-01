//! Hand-built VIR for static relocations (vtables) and the dynamic memory statements
//! (`MemCopyDyn`, `MemSet`): verifier rules and interpreter semantics.

use super::interp;
use crate::vir::*;

fn local(l: u32) -> Operand {
    Operand::Copy(Place::local(Local(l)))
}

fn int(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}

fn assign(l: u32, rv: Rvalue) -> Stmt {
    Stmt::Assign(Place::local(Local(l)), rv)
}

fn load(l: u32, ty: Ty) -> Rvalue {
    Rvalue::Use(Operand::Copy(Place {
        local: Local(l),
        proj: vec![Proj::Deref(ty)],
    }))
}

fn leaf(symbol: &str, v: i128) -> Function {
    Function {
        symbol: symbol.into(),
        params: vec![],
        ret: Ty::I64,
        locals: vec![],
        blocks: vec![BasicBlock {
            stmts: vec![],
            term: Terminator::Return(int(v, Ty::I64)),
        }],
        linkage: Linkage::Internal,
        locs: vec![],
        param_attrs: vec![],
        is_poll: false,
    }
}

fn indirect_call(target: u32, dest: u32, next: u32) -> Terminator {
    Terminator::Call {
        callee: Callee::Ptr {
            target: local(target),
            params: vec![],
            ret: Ty::I64,
        },
        args: vec![],
        dest: Some(Place::local(Local(dest))),
        next: BlockId(next),
    }
}

/// `velt_main` calls both slots of a vtable static (fn#1 → 4, fn#2 → 2), reads a byte through a
/// static→static reloc, then checks memmove overlap and memset on a 16-byte stack buffer.
/// Returns 42 on success, 1..=3 for the failed check.
pub(super) fn vtable_program() -> Program {
    let buf = Ty::Agg(AggId(1));
    let tys = [
        Ty::Ptr,  // 0 vtable
        Ty::Ptr,  // 1 slot fn
        Ty::I64,  // 2 f1()
        Ty::I64,  // 3 f2()
        buf,      // 4 buffer
        Ty::Ptr,  // 5 &buffer / scratch ptr
        Ty::Ptr,  // 6 scratch ptr
        Ty::Bool, // 7 check
        Ty::I64,  // 8 result
        Ty::U8,   // 9 byte
        Ty::I32,  // 10 exit code
    ];
    let locals = tys.iter().map(|&ty| LocalDecl { ty, name: None }).collect();
    let field = |n| {
        Operand::Copy(Place {
            local: Local(4),
            proj: vec![Proj::Field(n)],
        })
    };
    let check = |cond: Rvalue, next: u32, fail: u32| {
        (
            vec![assign(7, cond)],
            Terminator::Branch {
                cond: local(7),
                then: BlockId(next),
                els: BlockId(fail),
            },
        )
    };
    let (c1, t1) = check(
        Rvalue::Binary(BinOp::Eq, local(9), int(b'h' as i128, Ty::U8)),
        4,
        5,
    );
    let (c2, t2) = check(
        Rvalue::Binary(BinOp::Eq, field(0), int(0x0706_0504_0302_0101, Ty::U64)),
        6,
        7,
    );
    let (c3, t3) = check(
        Rvalue::Binary(BinOp::Eq, field(1), int(0xAAAA_AA08, Ty::U64)),
        8,
        9,
    );
    let ptr_add = |l, off| Rvalue::Binary(BinOp::PtrAdd, local(l), int(off, Ty::U64));
    let blocks = vec![
        BasicBlock {
            stmts: vec![
                assign(
                    0,
                    Rvalue::Use(Operand::Const(Const::Static(StaticId(0)), Ty::Ptr)),
                ),
                assign(1, load(0, Ty::Ptr)),
            ],
            term: indirect_call(1, 2, 1),
        },
        BasicBlock {
            stmts: vec![assign(6, ptr_add(0, 8)), assign(1, load(6, Ty::Ptr))],
            term: indirect_call(1, 3, 2),
        },
        BasicBlock {
            stmts: vec![
                assign(6, ptr_add(0, 16)),
                assign(6, load(6, Ty::Ptr)),
                assign(9, load(6, Ty::U8)),
            ],
            term: Terminator::Goto(BlockId(3)),
        },
        BasicBlock {
            stmts: [
                vec![
                    assign(
                        4,
                        Rvalue::Aggregate(
                            AggId(1),
                            vec![int(0x0807_0605_0403_0201, Ty::U64), int(0, Ty::U64)],
                        ),
                    ),
                    assign(5, Rvalue::AddrOf(Place::local(Local(4)))),
                    assign(6, ptr_add(5, 1)),
                    Stmt::MemCopyDyn {
                        dst: local(6),
                        src: local(5),
                        len: int(8, Ty::U64),
                        overlapping: true,
                    },
                    assign(6, ptr_add(5, 9)),
                    Stmt::MemSet {
                        dst: local(6),
                        byte: int(0xAA, Ty::U8),
                        len: int(3, Ty::U64),
                    },
                ],
                c1,
            ]
            .concat(),
            term: t1,
        },
        BasicBlock {
            stmts: c2,
            term: t2,
        },
        ret_block(1),
        BasicBlock {
            stmts: c3,
            term: t3,
        },
        ret_block(2),
        BasicBlock {
            stmts: vec![
                assign(8, Rvalue::Binary(BinOp::Mul, local(2), int(10, Ty::I64))),
                assign(8, Rvalue::Binary(BinOp::Add, local(8), local(3))),
                assign(10, Rvalue::Cast(local(8), Ty::I32)),
            ],
            term: Terminator::Return(local(10)),
        },
        ret_block(3),
    ];
    let main = Function {
        symbol: "velt_main".into(),
        params: vec![],
        ret: Ty::I32,
        locals,
        blocks,
        linkage: Linkage::Export,
        locs: vec![],
        param_attrs: vec![],
        is_poll: false,
    };
    Program {
        aggs: vec![
            AggLayout {
                name: "string".into(),
                size: 24,
                align: 8,
                fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
            },
            AggLayout {
                name: "Buf".into(),
                size: 16,
                align: 8,
                fields: vec![(Ty::U64, 0), (Ty::U64, 8)],
            },
        ],
        funcs: vec![main, leaf("f1", 4), leaf("f2", 2)],
        externs: vec![],
        statics: vec![
            StaticData {
                bytes: vec![0; 24],
                align: 8,
                relocs: vec![
                    (0, Const::Func(FuncId(1))),
                    (8, Const::Func(FuncId(2))),
                    (16, Const::Static(StaticId(1))),
                ],
            },
            StaticData {
                bytes: b"hi".to_vec(),
                align: 1,
                relocs: vec![],
            },
        ],
        files: vec![],
    }
}

fn ret_block(code: i128) -> BasicBlock {
    BasicBlock {
        stmts: vec![],
        term: Terminator::Return(int(code, Ty::I32)),
    }
}

fn expect_err(p: &Program, needle: &str) {
    match crate::verify(p) {
        Ok(()) => panic!("expected verify error containing {needle:?}"),
        Err(errs) => assert!(errs.iter().any(|e| e.contains(needle)), "{errs:?}"),
    }
}

#[test]
fn vtable_and_mem_statements_verify_and_run() {
    let p = vtable_program();
    if let Err(errs) = crate::verify(&p) {
        panic!("{}\n{p}", errs.join("\n"));
    }
    assert_eq!(interp::run(&p).code, 42, "{p}");
}

#[test]
fn verify_rejects_bad_relocs() {
    let bad: [(u32, Const, &str); 5] = [
        (4, Const::Func(FuncId(1)), "not 8-aligned"),
        (24, Const::Func(FuncId(1)), "out of bounds"),
        (8, Const::Func(FuncId(9)), "unknown function"),
        (8, Const::Int(0), "not an address"),
        (8, Const::Static(StaticId(1)), "overlapping"),
    ];
    for (off, target, needle) in bad {
        let mut p = vtable_program();
        let relocs = &mut p.statics[0].relocs;
        relocs.retain(|(o, _)| *o != 8 || needle == "overlapping");
        relocs.push((off, target));
        expect_err(&p, needle);
    }
    let mut p = vtable_program();
    p.statics[0].bytes[3] = 1;
    expect_err(&p, "not zero");
}

#[test]
fn verify_rejects_mistyped_mem_statements() {
    let mut p = vtable_program();
    p.funcs[0].blocks[3].stmts[5] = Stmt::MemSet {
        dst: local(6),
        byte: int(0xAA, Ty::I32),
        len: int(3, Ty::U64),
    };
    expect_err(&p, "memset byte has type I32, expected U8");
    let mut p = vtable_program();
    p.funcs[0].blocks[3].stmts[3] = Stmt::MemCopyDyn {
        dst: local(6),
        src: local(2),
        len: int(8, Ty::I64),
        overlapping: false,
    };
    expect_err(&p, "memcopy source has type I64, expected Ptr");
}
