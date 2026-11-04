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
    assert!(!ir.contains("@.s0") && !ir.contains("@.s1"), "{ir}");
    // Unnamed, they stay private, as before.
    let plain = emit_ir(&super::vtable_sample(), "x86_64-unknown-linux-gnu").unwrap();
    assert!(
        plain.contains("@.s1 = private unnamed_addr constant"),
        "{plain}"
    );
}
