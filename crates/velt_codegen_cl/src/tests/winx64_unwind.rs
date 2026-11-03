//! Windows x64 unwind records (`.xdata`) against the prologues they describe: every register a
//! prologue saves must be where its record says, at `rbp - 16 × frame offset + scaled offset`.
//! A structured exception (a Velt panic unwinding to `catchPanic`) restores the callee-saved
//! registers from there, so a wrong offset hands the frame that catches garbage (#2008).

use object::{Object, ObjectSection, ObjectSymbol};

use super::programs;
use super::*;
use crate::{emit_object, CodegenOptions};

/// `keeps(x, y)`: eight integers and twelve floats live across a call, so the prologue saves
/// integer and XMM callee-saved registers.
fn keeps_registers() -> Program {
    let (mut pb, rt) = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("keeps", &[Ty::I64, Ty::F64], Ty::F64);
    let b = fb.block();
    let ints: Vec<Local> = (0..8).map(|_| fb.local(Ty::I64)).collect();
    let floats: Vec<Local> = (0..12).map(|_| fb.local(Ty::F64)).collect();
    for (k, &l) in ints.iter().enumerate() {
        let rv = Rvalue::Binary(
            BinOp::Mul,
            copy_local(fb.param(0)),
            int(k as i128 + 3, Ty::I64),
        );
        fb.assign(b, l, rv);
    }
    for (k, &l) in floats.iter().enumerate() {
        let rv = Rvalue::Binary(
            BinOp::Mul,
            copy_local(fb.param(1)),
            float(k as f64 + 3.0, Ty::F64),
        );
        fb.assign(b, l, rv);
    }
    let args = vec![int(1, Ty::U32), copy_local(fb.param(0))];
    let mut cur = fb.call(b, Callee::Extern(rt.write_i64), args, None);
    let (isum, fsum) = (fb.local(Ty::I64), fb.local(Ty::F64));
    fb.assign(cur, isum, Rvalue::Use(int(0, Ty::I64)));
    fb.assign(cur, fsum, Rvalue::Use(float(0.0, Ty::F64)));
    for &l in &ints {
        fb.assign(
            cur,
            isum,
            Rvalue::Binary(BinOp::Add, copy_local(isum), copy_local(l)),
        );
    }
    for &l in &floats {
        fb.assign(
            cur,
            fsum,
            Rvalue::Binary(BinOp::Add, copy_local(fsum), copy_local(l)),
        );
    }
    let wide = fb.local(Ty::F64);
    fb.assign(cur, wide, Rvalue::Cast(copy_local(isum), Ty::F64));
    fb.assign(
        cur,
        fsum,
        Rvalue::Binary(BinOp::Add, copy_local(fsum), copy_local(wide)),
    );
    let ret = Terminator::Return(copy_local(fsum));
    fb.term(cur, ret);
    let keeps = pb.add(fb.finish());

    let (mut main, b) = main_fb();
    let r = main.local(Ty::F64);
    let args = vec![int(2, Ty::I64), float(0.5, Ty::F64)];
    cur = main.call(b, Callee::Func(keeps), args, Some(Place::local(r)));
    pb.add(finish_main(main, cur, 0));
    pb.p
}

/// One register save, as the prologue does it or as the record describes it: (register,
/// is XMM, offset from `rbp`).
type Save = (u8, bool, i64);

/// The saves a Cranelift x64 prologue makes: `push rbp; mov rbp, rsp; sub rsp, N`, then one
/// `mov [rsp + d], r64` or `movdqu [rsp + d], xmm` per register.
fn prologue_saves(code: &[u8]) -> Vec<Save> {
    assert_eq!(
        code[..4],
        [0x55, 0x48, 0x89, 0xE5],
        "push rbp; mov rbp, rsp"
    );
    let (n, mut at) = match code[4..7] {
        [0x48, 0x81, 0xEC] => (
            i64::from(u32::from_le_bytes(code[7..11].try_into().unwrap())),
            11,
        ),
        [0x48, 0x83, 0xEC] => (i64::from(code[7]), 8),
        _ => (0, 4),
    };
    let mut saves = Vec::new();
    loop {
        let xmm = code[at] == 0xF3;
        let mut i = at + usize::from(xmm);
        let rex = if code[i] & 0xF0 == 0x40 { code[i] } else { 0 };
        i += usize::from(rex != 0);
        let opcode: &[u8] = if xmm { &[0x0F, 0x7F] } else { &[0x89] };
        let modrm_at = i + opcode.len();
        if code[i..modrm_at] != *opcode || code[modrm_at] & 0x07 != 4 || code[modrm_at + 1] != 0x24
        {
            return saves;
        }
        let modrm = code[modrm_at];
        let disp_at = modrm_at + 2;
        let (disp, end) = match modrm >> 6 {
            0 => (0, disp_at),
            1 => (i64::from(code[disp_at] as i8), disp_at + 1),
            2 => (
                i64::from(i32::from_le_bytes(
                    code[disp_at..disp_at + 4].try_into().unwrap(),
                )),
                disp_at + 4,
            ),
            _ => return saves,
        };
        let reg = ((modrm >> 3) & 7) | if rex & 0x04 != 0 { 8 } else { 0 };
        saves.push((reg, xmm, disp - n));
        at = end;
    }
}

/// The saves an `UNWIND_INFO` record describes, with frame register `rbp`.
fn record_saves(record: &[u8]) -> Vec<Save> {
    assert_eq!(record[0], 1, "version 1, no handler");
    let count = usize::from(record[2]);
    let base = -16 * i64::from(record[3] >> 4);
    let nodes: Vec<[u8; 2]> = (0..count)
        .map(|i| [record[4 + 2 * i], record[5 + 2 * i]])
        .collect();
    let node = |i: usize| i64::from(u16::from_le_bytes(nodes[i]));
    let mut saves = Vec::new();
    let mut i = 0;
    while i < count {
        let (op, info) = (nodes[i][1] & 0x0F, nodes[i][1] >> 4);
        i += match op {
            0 | 2 | 3 => 1,
            1 => 2 + usize::from(info),
            4 => {
                saves.push((info, false, base + 8 * node(i + 1)));
                2
            }
            8 => {
                saves.push((info, true, base + 16 * node(i + 1)));
                2
            }
            5 | 9 => {
                saves.push((info, op == 9, base + (node(i + 1) | node(i + 2) << 16)));
                3
            }
            _ => panic!("unexpected unwind code {op}"),
        };
    }
    if !saves.is_empty() {
        assert_eq!(record[3] & 0x0F, 5, "frame register rbp");
    }
    saves
}

#[test]
fn windows_x64_records_find_saved_registers() {
    let mut checked = (0, 0);
    let mut programs: Vec<Program> = programs::all().into_iter().map(|t| t.program).collect();
    programs.push(keeps_registers());
    for program in programs {
        let opts = CodegenOptions {
            target: "x86_64-pc-windows-msvc".into(),
            optimize: false,
            cpu: None,
        };
        let bytes = emit_object(&program, &opts).unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let pdata = file.section_by_name(".pdata").unwrap();
        let pdata_bytes = pdata.data().unwrap();
        let xdata = file.section_by_name(".xdata").unwrap().data().unwrap();
        // `{ begin, end, unwind }` entries; the addends are implicit, in the section data.
        let mut functions = std::collections::HashMap::new();
        let mut records = std::collections::HashMap::new();
        for (offset, reloc) in pdata.relocations() {
            let object::RelocationTarget::Symbol(symbol) = reloc.target() else {
                panic!("pdata relocation without a symbol");
            };
            let at = offset as usize;
            let addend = u32::from_le_bytes(pdata_bytes[at..at + 4].try_into().unwrap());
            match at % 12 {
                0 => drop(functions.insert(at / 12, symbol)),
                8 => drop(records.insert(at / 12, addend as usize)),
                _ => {}
            }
        }
        for (entry, symbol) in functions {
            let symbol = file.symbol_by_index(symbol).unwrap();
            let text = file
                .section_by_index(symbol.section_index().unwrap())
                .unwrap();
            let code = &text.data().unwrap()[symbol.address() as usize..];
            let mut expected = prologue_saves(code);
            let mut described = record_saves(&xdata[records[&entry]..]);
            expected.sort();
            described.sort();
            assert_eq!(described, expected, "{}", symbol.name().unwrap());
            checked.0 += expected.iter().filter(|s| !s.1).count();
            checked.1 += expected.iter().filter(|s| s.1).count();
        }
    }
    assert!(
        checked.0 > 0 && checked.1 > 0,
        "integer and XMM saves checked: {checked:?}"
    );
}
