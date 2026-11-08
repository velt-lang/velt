//! Named statics (vir invariant 13): an `Export` static is the link's one copy under its symbol,
//! an `Import` one is another object's, with its contents for reading only.

use velt_vir::vir::*;

use crate::emit_ir;

#[test]
fn named_statics_are_hidden_globals_and_imports_are_available_externally() {
    let mut p = super::vtable_sample();
    p.statics[1].symbol = Some("_Vsvt".into());
    p.statics[1].linkage = Linkage::Export;
    p.statics[0].symbol = Some("_Vstext".into());
    p.statics[0].linkage = Linkage::Import;
    velt_vir::verify(&p).unwrap();
    let ir = emit_ir(&p, "x86_64-unknown-linux-gnu").unwrap();
    for needle in [
        "@\"_Vsvt\" = hidden unnamed_addr constant <{ [8 x i8], ptr, ptr, ptr, [8 x i8] }>",
        "ptr @\"_Vstext\", [8 x i8]",
        "@\"_Vstext\" = available_externally hidden unnamed_addr constant [2 x i8] c\"hi\"",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    assert!(!ir.contains("@.s."), "{ir}");
    // Unnamed, they stay private, as before.
    let unnamed = super::vtable_sample();
    let plain = emit_ir(&unnamed, "x86_64-unknown-linux-gnu").unwrap();
    let private = format!(
        "{} = private unnamed_addr constant",
        super::static_name(&unnamed, 1)
    );
    assert!(plain.contains(&private), "{plain}");
}

/// An unnamed static's name comes from its contents, not its index: a static added in front
/// (a new string of the program) renames none of the others, so the modules of codegen units
/// that use only those stay the same (and come from the build cache).
#[test]
fn unnamed_statics_keep_their_names_when_others_are_added() {
    let p = super::vtable_sample();
    let names = crate::statics::static_names(&p);
    let mut q = p.clone();
    q.statics.insert(
        0,
        velt_vir::vir::StaticData {
            bytes: b"a new string".to_vec(),
            align: 1,
            relocs: vec![],
            mutable: None,
            symbol: None,
            linkage: Linkage::Internal,
        },
    );
    // References to the old statics move up by one.
    for s in &mut q.statics {
        for (_, target) in &mut s.relocs {
            if let velt_vir::vir::Const::Static(id) = target {
                id.0 += 1;
            }
        }
    }
    let renamed = crate::statics::static_names(&q);
    assert_eq!(
        &renamed[1..],
        &names[..],
        "the old statics keep their names"
    );
    assert!(!names.contains(&renamed[0]));
    // Equal contents get distinct names, in order.
    let mut twins = p.clone();
    twins.statics.push(p.statics[0].clone());
    let twin_names = crate::statics::static_names(&twins);
    let last = twin_names.last().expect("statics");
    assert_eq!(*last, format!("{}.1", names[0]));
}
