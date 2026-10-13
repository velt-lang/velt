//! Where `allocate_locals` puts a function's locals: described scalars in registers with a
//! shadow slot (#1030), memory locals in slots of their own, other scalars in registers.

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use velt_vir::vir::{self, DebugTyId, LocalDebug, LocalDecl, Program, SrcLoc, Ty, STR_AGG};

use super::{allocate_locals, Storage};

fn described(ty: Ty) -> LocalDecl {
    LocalDecl {
        debug: Some(LocalDebug {
            decl: SrcLoc {
                file: 0,
                line: 1,
                col: 1,
            },
            end: None,
            ty: DebugTyId(0),
            by_ref: false,
            param: false,
        }),
        ..LocalDecl::new(ty, Some("x".into()))
    }
}

#[test]
fn described_scalars_keep_registers_and_get_a_shadow() {
    let program = Program {
        aggs: vec![crate::tests::str_agg()],
        ..Default::default()
    };
    // 0: described, assigned once; 1: described, assigned twice; 2: a described string (in
    // memory); 3: a scalar without a description.
    let function = vir::Function {
        locals: vec![
            described(Ty::I64),
            described(Ty::F64),
            described(Ty::Agg(STR_AGG)),
            LocalDecl::new(Ty::I64, None),
        ],
        ..vir::Function::new("f".into(), vec![], Ty::Unit, vir::Linkage::Internal)
    };
    let in_memory = [false, false, true, false];
    let single = [true, false, false, false];
    let mut func = ir::Function::new();
    let mut ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut ctx);
    let (storage, shadows) =
        allocate_locals(&mut builder, &program, &function, &in_memory, &single, true).unwrap();
    assert!(matches!(storage[0], Storage::Value(0)));
    assert!(shadows.by_local.contains_key(&0));
    let Storage::Var(var) = storage[1] else {
        panic!("a described local assigned twice is a variable")
    };
    assert!(shadows.by_var.contains_key(&var.index()));
    assert!(matches!(storage[2], Storage::Slot(_)));
    assert!(!shadows.by_local.contains_key(&2));
    let Storage::Var(plain) = storage[3] else {
        panic!("an undescribed scalar is a variable")
    };
    assert!(!shadows.by_var.contains_key(&plain.index()));
    assert_eq!(shadows.by_local.len() + shadows.by_var.len(), 2);

    // Without DWARF variables for the target, nothing gets a shadow.
    let (_, shadows) = allocate_locals(
        &mut builder,
        &program,
        &function,
        &in_memory,
        &single,
        false,
    )
    .unwrap();
    assert!(shadows.by_local.is_empty() && shadows.by_var.is_empty());
}
