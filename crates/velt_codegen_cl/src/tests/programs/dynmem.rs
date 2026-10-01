//! Programs for static relocations (a vtable of function/extern addresses, a pointer to another
//! static) and runtime-length memory statements (`MemCopyDyn`, `MemSet`).

use super::*;
use crate::tests::*;
use velt_vir::vir::Ty::*;

fn ptr_add(base: Local, offset: i128) -> Rvalue {
    bin(BinOp::PtrAdd, copy_local(base), int(offset, U64))
}

fn load(p: Local, ty: Ty) -> Rvalue {
    Rvalue::Use(copy_place(place(p, vec![Proj::Deref(ty)])))
}

/// Calls both `(I64, I64) -> I64` slots of a vtable static and an extern slot through their
/// relocated addresses, then reads a byte through a static → static pointer.
pub(crate) fn vtables() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let (add, _) = binary_fn(&mut pb, BinOp::Add, I64, I64);
    let (mul, _) = binary_fn(&mut pb, BinOp::Mul, I64, I64);
    let digits = pb.stat(b"0123456789");
    let vtable = pb.stat_with(
        &[0; 24],
        8,
        vec![
            (0, Const::Func(add)),
            (8, Const::Func(mul)),
            (16, Const::Extern(rt.write_i64)),
        ],
    );
    let holder = pb.stat_with(&[0; 8], 8, vec![(0, Const::Static(digits))]);

    let (mut fb, b0) = main_fb();
    let (vt, slot, fp, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr), fb.local(I64));
    let byte = fb.local(U8);
    let mut out = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    out.fb.assign(
        b0,
        vt,
        Rvalue::Use(Operand::Const(Const::Static(vtable), Ptr)),
    );
    for offset in [0, 8] {
        let cur = out.cur;
        out.fb.assign(cur, slot, ptr_add(vt, offset));
        out.fb.assign(cur, fp, load(slot, Ptr));
        out.cur = out.fb.call(
            cur,
            Callee::Ptr {
                target: copy_local(fp),
                params: vec![I64, I64],
                ret: I64,
            },
            vec![int(6, I64), int(7, I64)],
            Some(Place::local(r)),
        );
        out.line(copy_local(r), I64);
    }
    let cur = out.cur;
    out.fb.assign(cur, slot, ptr_add(vt, 16));
    out.fb.assign(cur, fp, load(slot, Ptr));
    out.cur = out.fb.call(
        cur,
        Callee::Ptr {
            target: copy_local(fp),
            params: vec![U32, I64],
            ret: Unit,
        },
        vec![int(1, U32), int(-5, I64)],
        None,
    );
    out.nl();
    let cur = out.cur;
    out.fb.assign(
        cur,
        slot,
        Rvalue::Use(Operand::Const(Const::Static(holder), Ptr)),
    );
    out.fb.assign(cur, slot, load(slot, Ptr));
    out.fb.assign(cur, slot, ptr_add(slot, 3));
    out.fb.assign(cur, byte, load(slot, U8));
    out.line(copy_local(byte), U8);
    let cur = out.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "vtables",
        program: pb.p,
        stdout: "13\n42\n-5\n51\n".into(),
        exit: 0,
    }
}

/// The byte-level effect of `mem_ops` on a 32-byte buffer, as the four U64 fields it prints
/// after each step.
fn mem_ops_expected() -> String {
    let mut buf = [0u8; 32];
    let mut lines = String::new();
    let mut show = |buf: &[u8; 32]| {
        for chunk in buf.chunks(8) {
            let v = u64::from_le_bytes(chunk.try_into().unwrap());
            lines.push_str(&format!("{v}\n"));
        }
    };
    buf.fill(0x41);
    show(&buf);
    let init = |buf: &mut [u8; 32]| {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = if i < 16 { i as u8 } else { 0 };
        }
    };
    init(&mut buf);
    buf.copy_within(0..16, 1);
    show(&buf);
    init(&mut buf);
    buf.copy_within(3..19, 0);
    show(&buf);
    buf.copy_within(0..16, 16);
    buf[29..32].fill(0xFE);
    show(&buf);
    lines
}

/// memset, forward/backward overlapping memmove and memcpy on a stack buffer, with lengths
/// held in locals so they are runtime values.
pub(crate) fn mem_ops() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let quad = pb.agg(AggLayout {
        name: "Quad".into(),
        size: 32,
        align: 8,
        fields: vec![(U64, 0), (U64, 8), (U64, 16), (U64, 24)],
    });
    let (mut fb, b0) = main_fb();
    let (buf, a, p, n) = (
        fb.local(Agg(quad)),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(U64),
    );
    let mut out = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    let init = Rvalue::Aggregate(
        quad,
        vec![
            int(0x0706_0504_0302_0100, U64),
            int(0x0f0e_0d0c_0b0a_0908, U64),
            int(0, U64),
            int(0, U64),
        ],
    );
    let show = |out: &mut Out| {
        for field in 0..4 {
            out.line(copy_place(place(buf, vec![Proj::Field(field)])), U64);
        }
    };
    out.fb.assign(b0, a, Rvalue::AddrOf(Place::local(buf)));
    out.fb.assign(b0, n, Rvalue::Use(int(32, U64)));
    out.fb.push(
        b0,
        Stmt::MemSet {
            dst: copy_local(a),
            byte: int(0x41, U8),
            len: copy_local(n),
        },
    );
    show(&mut out);
    let steps = [(1, 0, true), (0, 3, true), (16, 0, false)];
    for (i, (dst, src, overlapping)) in steps.into_iter().enumerate() {
        let cur = out.cur;
        if i < 2 {
            out.fb.assign(cur, buf, init.clone());
        }
        let mut operand_at = |offset: i128| {
            let l = out.fb.local(Ptr);
            out.fb.assign(cur, l, ptr_add(a, offset));
            copy_local(l)
        };
        let (dst, src) = (operand_at(dst), operand_at(src));
        out.fb.assign(cur, n, Rvalue::Use(int(16, U64)));
        out.fb.push(
            cur,
            Stmt::MemCopyDyn {
                dst,
                src,
                len: copy_local(n),
                overlapping,
            },
        );
        if !overlapping {
            out.fb.assign(cur, p, ptr_add(a, 29));
            out.fb.assign(cur, n, Rvalue::Use(int(3, U64)));
            out.fb.push(
                cur,
                Stmt::MemSet {
                    dst: copy_local(p),
                    byte: int(0xFE, U8),
                    len: copy_local(n),
                },
            );
        }
        show(&mut out);
    }
    let cur = out.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "mem_ops",
        program: pb.p,
        stdout: mem_ops_expected(),
        exit: 0,
    }
}
