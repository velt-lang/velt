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
        let err = shares_nothing(&calls).unwrap_err();
        assert!(err.contains(symbol), "{err}");
        let address = Terminator::Return(Operand::Const(Const::Extern(ExternId(1)), Ty::Ptr));
        let takes = program(vec![body("_V3stdN1f", address)], &externs, vec![]);
        assert!(shares_nothing(&takes).is_err(), "{symbol}");
        let listed = program(vec![body("_V3stdN1f", call(0))], &externs, vec![]);
        assert_eq!(shares_nothing(&listed), Ok(()), "{symbol}");
    }
}

/// The executor's module hooks hold module state: a body of one is refused, a declaration not.
#[test]
fn a_module_hook_of_the_executor_is_refused() {
    let symbol = velt_vir::mangle::mangle("velt/async/sched_hooks::schedModuleInit");
    let with_body = body(&symbol, Terminator::Goto(BlockId(1)));
    let err = shares_nothing(&program(vec![with_body], &[], vec![])).unwrap_err();
    assert!(err.contains("schedModule"), "{err}");
    let declared = vir::Function::new(symbol, vec![], Ty::Unit, Linkage::Import);
    assert_eq!(
        shares_nothing(&program(vec![declared], &[], vec![])),
        Ok(())
    );
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
        assert!(shares_nothing(&direct).is_err(), "{shared:?}");
        let statics = vec![literal(), table.clone(), shared.clone()];
        let through = program(vec![names_static(1)], &[], statics);
        assert!(
            shares_nothing(&through).is_err(),
            "{shared:?} through a relocation"
        );
        let unnamed = program(vec![names_static(0)], &[], vec![literal(), shared.clone()]);
        assert_eq!(
            shares_nothing(&unnamed),
            Ok(()),
            "{shared:?} named by nothing"
        );
    }
    let statics = vec![literal(), table, literal()];
    assert_eq!(
        shares_nothing(&program(vec![names_static(1)], &[], statics)),
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
    assert!(shares_nothing(&p).is_err());
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
