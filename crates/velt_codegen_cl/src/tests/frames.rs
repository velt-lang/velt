//! Frame sizes (#2048): memory locals whose live ranges are disjoint share a stack slot, so a
//! long function of temporaries keeps a small frame.

use object::{Object, ObjectSection, ObjectSymbol};

use super::*;
use crate::{emit_object, CodegenOptions};

/// The stack adjustment in the prologue of `symbol` in an x86-64 ELF object: the immediate of
/// its first `sub rsp, imm` (`48 83 ec ib` or `48 81 ec id`), 0 without one.
fn frame_size(bytes: &[u8], symbol: &str) -> u32 {
    let file = object::File::parse(bytes).unwrap();
    let sym = file
        .symbols()
        .find(|s| s.name() == Ok(symbol))
        .unwrap_or_else(|| panic!("{symbol} missing"));
    let section = file.section_by_index(sym.section_index().unwrap()).unwrap();
    let data = section.data().unwrap();
    let start = (sym.address() - section.address()) as usize;
    let code = &data[start..start + sym.size() as usize];
    let prologue = &code[..code.len().min(32)];
    for i in 0..prologue.len().saturating_sub(3) {
        match prologue[i..] {
            [0x48, 0x83, 0xec, b, ..] => return u32::from(b),
            [0x48, 0x81, 0xec, a, b, c, d, ..] => return u32::from_le_bytes([a, b, c, d]),
            _ => {}
        }
    }
    0
}

/// `fn f() -> u64`: `n` string temporaries, each built, read and dead before the next.
fn temporaries(n: usize) -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    let mut fb = FuncBuilder::new("many_temporaries", &[], Ty::U64, Linkage::Export);
    let sum = fb.local(Ty::U64);
    let b = fb.block();
    fb.assign(b, sum, Rvalue::Use(int(0, Ty::U64)));
    for i in 0..n {
        let t = fb.local(Ty::Agg(STR_AGG));
        fb.assign(
            b,
            t,
            Rvalue::Aggregate(
                STR_AGG,
                vec![int(i as i128, Ty::U64), int(1, Ty::U64), int(2, Ty::U64)],
            ),
        );
        let field = Operand::Copy(place(t, vec![Proj::Field(0)]));
        fb.assign(b, sum, Rvalue::Binary(BinOp::Add, copy_local(sum), field));
    }
    fb.term(b, Terminator::Return(copy_local(sum)));
    pb.add(fb.finish());
    let (main, b) = main_fb();
    pb.add(finish_main(main, b, 0));
    pb.p
}

#[test]
fn disjoint_temporaries_share_their_slot() {
    // Debug builds (no Cranelift optimization). One slot of its own per temporary made this
    // frame 24 KB.
    let program = temporaries(1000);
    let opts = CodegenOptions {
        target: "x86_64-unknown-linux-gnu".into(),
        optimize: false,
        cpu: None,
    };
    let bytes = emit_object(&program, &opts).unwrap();
    let size = frame_size(&bytes, "many_temporaries");
    assert!(
        size <= 64,
        "frame of 1000 disjoint temporaries is {size} bytes"
    );
}
