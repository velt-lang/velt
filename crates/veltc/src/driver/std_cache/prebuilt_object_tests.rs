//! The object's refusals on hand-built programs: state a program shares ([`shares_nothing`]),
//! and entry points lowered otherwise than values cross ([`crosses`]).

use super::*;
use velt_common::hook_sig::{FnSig, Param};
use velt_vir::vir::{BasicBlock, BlockId, ExternFn, ExternId, StaticData, StaticId};

/// A function with a body ending in `term`, then a return.
fn body(symbol: &str, term: Terminator) -> vir::Function {
    let mut f = vir::Function::new(symbol.into(), vec![], Ty::Unit, Linkage::Internal);
    f.blocks = vec![
        BasicBlock {
            stmts: vec![],
            term,
        },
        BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Const(Const::Unit, Ty::Unit)),
        },
    ];
    f
}

fn call(e: u32) -> Terminator {
    Terminator::Call {
        callee: Callee::Extern(ExternId(e)),
        args: vec![],
        dest: None,
        next: BlockId(1),
        unwind: None,
    }
}

/// A function that takes the address of static `s` (and returns it).
fn names_static(s: u32) -> vir::Function {
    let mut f = body("_V3stdN1g", Terminator::Goto(BlockId(1)));
    f.ret = Ty::Ptr;
    f.blocks[1].term = Terminator::Return(Operand::Const(Const::Static(StaticId(s)), Ty::Ptr));
    f
}

fn extern_fn(symbol: &str) -> ExternFn {
    ExternFn {
        symbol: symbol.into(),
        params: vec![],
        ret: Ty::Unit,
        noreturn: false,
        host: None,
        c_decl: None,
        variadic: false,
        c_abi: None,
    }
}

/// A read-only internal static (a string literal's bytes).
fn literal() -> StaticData {
    StaticData {
        bytes: b"text".to_vec(),
        align: 1,
        ..StaticData::default()
    }
}

fn program(funcs: Vec<vir::Function>, externs: &[&str], statics: Vec<StaticData>) -> vir::Program {
    vir::Program {
        funcs,
        externs: externs.iter().map(|s| extern_fn(s)).collect(),
        statics,
        ..vir::Program::default()
    }
}

/// Each extern a program shares state through is refused where the object's code calls it or
/// takes its address; one only listed is not.
#[test]
fn an_extern_of_shared_state_is_refused() {
    for symbol in [
        "velt_rt_module_state",
        "velt_tls_get_i64",
        "velt_rt_statics",
    ] {
        let externs = ["velt_rt_alloc", symbol];
        let calls = program(vec![body("_V3stdN1f", call(1))], &externs, vec![]);
        let err = shares_nothing(&calls, &HashSet::new()).unwrap_err();
        assert!(err.contains(symbol), "{err}");
        let address = Terminator::Return(Operand::Const(Const::Extern(ExternId(1)), Ty::Ptr));
        let takes = program(vec![body("_V3stdN1f", address)], &externs, vec![]);
        assert!(shares_nothing(&takes, &HashSet::new()).is_err(), "{symbol}");
        let listed = program(vec![body("_V3stdN1f", call(0))], &externs, vec![]);
        assert_eq!(shares_nothing(&listed, &HashSet::new()), Ok(()), "{symbol}");
    }
}

/// The executor's module hooks hold module state: a body of one is refused, a declaration not.
#[test]
fn a_module_hook_of_the_executor_is_refused() {
    let symbol = velt_vir::mangle::mangle("velt/async/sched_hooks::schedModuleInit");
    let with_body = body(&symbol, Terminator::Goto(BlockId(1)));
    let blocks = &[BLOCKS_MODULE][..];
    for private in [&[][..], blocks] {
        let p = program(vec![with_body.clone()], &[], vec![]);
        let err = holds_no_module_state(&p, private).unwrap_err();
        assert!(err.contains("schedModule"), "{err}");
    }
    let declared = vir::Function::new(symbol, vec![], Ty::Unit, Linkage::Import);
    assert_eq!(
        holds_no_module_state(&program(vec![declared], &[], vec![]), &[]),
        Ok(())
    );
}

/// The blocks of module state are the state of module_blocks.velt's object alone: its functions
/// are taken in that module's own object (its private list names it), and refused in any other
/// module's, where they would be a second copy of the blocks' table. A call of the module's entry
/// points is refused in every object: only a program that reaches module state links theirs.
#[test]
fn the_blocks_of_module_state_are_one_module_s_own() {
    let symbol = velt_vir::mangle::mangle("std/lang/hooks/module_blocks::newBlock");
    assert!(symbol.contains(BLOCKS_CODE), "{symbol}");
    let code = || {
        program(
            vec![body(&symbol, Terminator::Goto(BlockId(1)))],
            &[],
            vec![],
        )
    };
    assert_eq!(holds_no_module_state(&code(), &[BLOCKS_MODULE]), Ok(()));
    let weak = velt_common::std_hooks::prebuilt::private("lang_hooks_weak");
    for other in [&[][..], weak] {
        let err = holds_no_module_state(&code(), other).unwrap_err();
        assert!(err.contains(BLOCKS_CODE), "{err}");
    }
    // The table's own list is what `build` passes for the module.
    let own = velt_common::std_hooks::prebuilt::private("lang_hooks_module_blocks");
    assert_eq!(holds_no_module_state(&code(), own), Ok(()));
    for entry in ["velt_hook_module_state", "velt_hook_module_drop"] {
        let externs = ["velt_rt_alloc", entry];
        let calls = || program(vec![body("_V3stdN1f", call(1))], &externs, vec![]);
        for private in [&[][..], own] {
            let err = holds_no_module_state(&calls(), private).unwrap_err();
            assert!(err.contains(entry), "{err}");
        }
        let listed = program(vec![body("_V3stdN1f", call(0))], &externs, vec![]);
        assert_eq!(holds_no_module_state(&listed, &[]), Ok(()));
    }
}

/// A static the code names that a program could see or write too is refused: a static atomic,
/// a named one (exported or imported), each also when reached only through another static's
/// relocation. A read-only internal static is taken, and so is a shared one nothing names.
#[test]
fn a_mutable_or_named_static_is_refused() {
    let atomic = StaticData {
        bytes: vec![0; 8],
        align: 8,
        mutable: Some(7),
        ..StaticData::default()
    };
    let named = StaticData {
        symbol: Some("_Vs0123".into()),
        linkage: Linkage::Export,
        ..literal()
    };
    let imported = StaticData {
        symbol: Some("velt_rt_str_epoch".into()),
        linkage: Linkage::Import,
        ..literal()
    };
    // A vtable-like static whose relocation names static 2.
    let table = StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: vec![(0, Const::Static(StaticId(2)))],
        ..StaticData::default()
    };
    for shared in [atomic, named, imported] {
        let direct = program(vec![names_static(1)], &[], vec![literal(), shared.clone()]);
        assert!(
            shares_nothing(&direct, &HashSet::new()).is_err(),
            "{shared:?}"
        );
        let statics = vec![literal(), table.clone(), shared.clone()];
        let through = program(vec![names_static(1)], &[], statics);
        assert!(
            shares_nothing(&through, &HashSet::new()).is_err(),
            "{shared:?} through a relocation"
        );
        let unnamed = program(vec![names_static(0)], &[], vec![literal(), shared.clone()]);
        assert_eq!(
            shares_nothing(&unnamed, &HashSet::new()),
            Ok(()),
            "{shared:?} named by nothing"
        );
    }
    let statics = vec![literal(), table, literal()];
    assert_eq!(
        shares_nothing(
            &program(vec![names_static(1)], &[], statics),
            &HashSet::new()
        ),
        Ok(())
    );
}

/// A relocation of a static the code names can reach a shared extern too.
#[test]
fn an_extern_reached_through_a_relocation_is_refused() {
    let table = StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: vec![(0, Const::Extern(ExternId(0)))],
        ..StaticData::default()
    };
    let p = program(vec![names_static(0)], &["velt_tls_slot"], vec![table]);
    assert!(shares_nothing(&p, &HashSet::new()).is_err());
}

const F64: Param = Param {
    mode: Mode::Value,
    ty: T::F64,
};
const USIZE: Param = Param {
    mode: Mode::Value,
    ty: T::USize,
};
const TEXT: Param = Param {
    mode: Mode::Borrow,
    ty: T::Str,
};
const BOOL: Param = Param {
    mode: Mode::Value,
    ty: T::Bool,
};

fn sig(params: &'static [Param], ret: T) -> FnSig {
    FnSig {
        generics: 0,
        params,
        ret,
        throws: None,
        is_async: false,
    }
}

/// An entry point lowered as values cross (`prebuilt.rs`) is taken; any other lowering is
/// refused.
#[test]
fn an_entry_point_lowered_otherwise_is_refused() {
    let text = sig(&[F64, USIZE], T::Str);
    let value = sig(&[TEXT, USIZE], T::F64);
    let nothing = sig(&[F64], T::Void);
    assert!(crosses(&text, &[Ty::F64, Ty::U64, Ty::Ptr], Ty::Unit));
    assert!(crosses(&value, &[Ty::Ptr, Ty::U64], Ty::F64));
    assert!(crosses(&nothing, &[Ty::F64], Ty::Unit));
    // A `bool` crosses as `Ty::Bool`, and only as that.
    let flag = sig(&[BOOL, USIZE], T::Bool);
    assert!(crosses(&flag, &[Ty::Bool, Ty::U64], Ty::Bool));
    assert!(!crosses(&flag, &[Ty::U8, Ty::U64], Ty::Bool));
    assert!(!crosses(&flag, &[Ty::Bool, Ty::U64], Ty::U8));
    // A `&string` as a scalar.
    assert!(!crosses(&value, &[Ty::I64, Ty::U64], Ty::F64));
    // A number as a pointer, a `bool` or an aggregate; a `bool` result.
    assert!(!crosses(&value, &[Ty::Ptr, Ty::Ptr], Ty::F64));
    assert!(!crosses(&value, &[Ty::Ptr, Ty::Bool], Ty::F64));
    assert!(!crosses(
        &value,
        &[Ty::Ptr, Ty::Agg(vir::AggId(0))],
        Ty::F64
    ));
    assert!(!crosses(&value, &[Ty::Ptr, Ty::U64], Ty::Bool));
    // A `string` result without its trailing out-pointer, or returned besides.
    assert!(!crosses(&text, &[Ty::F64, Ty::U64, Ty::I64], Ty::Unit));
    assert!(!crosses(&text, &[Ty::F64, Ty::U64, Ty::Ptr], Ty::Ptr));
    assert!(!crosses(&text, &[Ty::F64, Ty::U64], Ty::Agg(vir::AggId(0))));
    // A `void` result that returns something.
    assert!(!crosses(&nothing, &[Ty::F64], Ty::I64));
    // Another number of parameters.
    assert!(!crosses(&value, &[Ty::Ptr], Ty::F64));
    assert!(!crosses(&value, &[Ty::Ptr, Ty::U64, Ty::U64], Ty::F64));
    assert!(!crosses(
        &text,
        &[Ty::F64, Ty::U64, Ty::Ptr, Ty::Ptr],
        Ty::Unit
    ));
}

/// The module's own state (`own_state`: its private modules' thread-local values and static
/// atomics) is taken; another constant's, a thread-local extern of another form, and a named
/// static are still refused.
#[test]
fn the_module_s_own_state_is_taken() {
    use velt_vir::tls::TlsCall;
    let own: HashSet<u64> = [7, 9].into();
    let get = |id| {
        TlsCall::Get {
            id,
            bits: 64,
            init: 0,
        }
        .symbol()
    };
    let (mine, other) = (get(7), get(8));
    let calls = program(vec![body("_V3stdN1f", call(0))], &[mine.as_str()], vec![]);
    assert_eq!(shares_nothing(&calls, &own), Ok(()));
    assert!(shares_nothing(&calls, &HashSet::new()).is_err());
    let calls = program(vec![body("_V3stdN1f", call(0))], &[other.as_str()], vec![]);
    assert!(shares_nothing(&calls, &own).is_err());
    let calls = program(
        vec![body("_V3stdN1f", call(0))],
        &["velt_tls_get_i64"],
        vec![],
    );
    assert!(shares_nothing(&calls, &own).is_err());
    let atomic = |id| StaticData {
        bytes: vec![0; 8],
        align: 8,
        mutable: Some(id),
        ..StaticData::default()
    };
    let p = program(vec![names_static(0)], &[], vec![atomic(9)]);
    assert_eq!(shares_nothing(&p, &own), Ok(()));
    let p = program(vec![names_static(0)], &[], vec![atomic(10)]);
    assert!(shares_nothing(&p, &own).is_err());
    let named = StaticData {
        symbol: Some("_Vs0123".into()),
        linkage: Linkage::Export,
        ..atomic(9)
    };
    let p = program(vec![names_static(0)], &[], vec![named]);
    assert!(shares_nothing(&p, &own).is_err());
}

/// A call of the program's writer (`prebuilt::calls_out`, #2196): its body, which reaches the
/// writer's thread-local buffer, is refused in the object; declared under its own symbol, the
/// object holds only the call. A call of any other function defined outside the object is
/// refused.
#[test]
fn the_writer_is_called_out_to() {
    let write = velt_vir::mangle::mangle(velt_common::std_hook!(write_str));
    let tls = velt_vir::tls::TlsCall::Get {
        id: 7,
        bits: 64,
        init: 0,
    }
    .symbol();
    let mut writer = vir::Function::new(
        write.clone(),
        vec![Ty::U32, Ty::Ptr],
        Ty::Unit,
        Linkage::Internal,
    );
    writer.blocks = body(&write, call(0)).blocks;
    writer.param_attrs = vec![Default::default(); 2];
    writer.param_attrs[1].readonly = true;
    let caller = |callee: u32| {
        body(
            "_V3stdN1f",
            Terminator::Call {
                callee: Callee::Func(vir::FuncId(callee)),
                args: vec![],
                dest: None,
                next: BlockId(1),
                unwind: None,
            },
        )
    };
    let mut p = program(vec![writer, caller(0)], &[tls.as_str()], vec![]);
    assert!(shares_nothing(&p, &HashSet::new()).is_err());
    let calls = declare_calls_out(&mut p).unwrap();
    assert_eq!(calls, std::slice::from_ref(&write));
    let declared = &p.funcs[0];
    assert!(declared.blocks.is_empty() && declared.linkage == Linkage::Import);
    assert!(!declared.param_attrs[1].readonly);
    assert_eq!(shares_nothing(&p, &HashSet::new()), Ok(()));
    assert_eq!(calls_nothing_else(&p, &calls), Ok(()));
    // Not called out to: refused.
    let err = calls_nothing_else(&p, &[]).unwrap_err();
    assert!(err.contains(&write), "{err}");
    let other = vir::Function::new("_V3stdN1g".into(), vec![], Ty::Unit, Linkage::Import);
    p.funcs.push(other);
    p.funcs.push(caller(2));
    let err = calls_nothing_else(&p, &calls).unwrap_err();
    assert!(err.contains("_V3stdN1g"), "{err}");
}
