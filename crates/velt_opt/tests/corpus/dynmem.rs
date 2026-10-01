//! Programs using static relocations (a vtable whose functions are reachable only through it)
//! and runtime-length memory statements (`MemSet`, `MemCopyDyn` in both directions).

use super::*;
use velt_vir::vir::Ty::*;

pub fn cases() -> Vec<Case> {
    vec![vtable_dispatch(), buffer_shuffle()]
}

/// `vt_dispatch(which, v)`: calls slot `which & 1` of a static vtable {double, triple}; the
/// slot functions have no other references, so only the relocations keep them alive.
fn vtable_dispatch() -> Case {
    let mut env = Env::new();
    // An unreferenced function first, so its removal renumbers the vtable targets.
    let mut fb = FuncBuilder::internal("unused", &[], Unit);
    let b = fb.block();
    fb.ret(b, unit());
    env.pb.add(fb.finish());
    let mut slots = vec![];
    for (name, k) in [("double", 2), ("triple", 3)] {
        let mut fb = FuncBuilder::internal(name, &[I64], I64);
        let r = fb.local(I64);
        let b = fb.block();
        fb.assign(b, r, bin(BinOp::Mul, copy_local(fb.param(0)), int(k, I64)));
        fb.ret(b, copy_local(r));
        slots.push(env.pb.add(fb.finish()));
    }
    let vtable = env.pb.stat_with(
        &[0; 16],
        8,
        vec![(0, Const::Func(slots[0])), (8, Const::Func(slots[1]))],
    );

    let mut fb = FuncBuilder::export("vt_dispatch", &[I64, I64], I64);
    let (which, v) = (fb.param(0), fb.param(1));
    let (off, slot, fp, r) = (fb.local(I64), fb.local(Ptr), fb.local(Ptr), fb.local(I64));
    let b = fb.block();
    fb.assign(b, off, bin(BinOp::BitAnd, copy_local(which), int(1, I64)));
    fb.assign(b, off, bin(BinOp::Mul, copy_local(off), int(8, I64)));
    fb.assign(
        b,
        slot,
        bin(
            BinOp::PtrAdd,
            Operand::Const(Const::Static(vtable), Ptr),
            copy_local(off),
        ),
    );
    fb.assign(b, fp, Rvalue::Use(copy_place(deref(slot, Ptr))));
    let callee = Callee::Ptr {
        target: copy_local(fp),
        params: vec![I64],
        ret: I64,
    };
    let b = fb.call(b, callee, vec![copy_local(v)], Some(r));
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    let inputs = [(0, 5), (1, 5), (7, -4)]
        .iter()
        .map(|&(w, v)| vec![arg(w), arg(v)])
        .collect();
    env.case("vtable_dispatch", "vt_dispatch", inputs)
}

/// `shuffle(n, b)`: fills a 32-byte buffer with byte `b`, writes a known pattern, then
/// memmoves `n & 15` bytes forwards and backwards and memcpys the low half up; prints all
/// four words. Constant lengths after inlining must not change anything.
fn buffer_shuffle() -> Case {
    let mut env = Env::new();
    let quad = env
        .pb
        .agg("Quad", 32, 8, &[(I64, 0), (I64, 8), (I64, 16), (I64, 24)]);
    let mut fb = FuncBuilder::export("shuffle", &[I64, I64], I64);
    let (n, byte_in) = (fb.param(0), fb.param(1));
    let (buf, a, p, len, byte) = (
        fb.local(Ty::Agg(quad)),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(U64),
        fb.local(U8),
    );
    let mut b = fb.block();
    fb.assign(b, a, Rvalue::AddrOf(Place::local(buf)));
    fb.assign(b, byte, Rvalue::Cast(copy_local(byte_in), U8));
    fb.push(
        b,
        Stmt::MemSet {
            dst: copy_local(a),
            byte: copy_local(byte),
            len: int(32, U64),
        },
    );
    fb.assign(b, p, bin(BinOp::PtrAdd, copy_local(a), int(8, U64)));
    fb.push(
        b,
        Stmt::Assign(deref(p, I64), Rvalue::Use(int(0x0102_0304_0506_0708, I64))),
    );
    fb.assign(b, len, Rvalue::Cast(copy_local(n), U64));
    fb.assign(b, len, bin(BinOp::BitAnd, copy_local(len), int(15, U64)));
    for (dst, src, overlapping) in [(3, 0, true), (0, 5, true), (16, 0, false)] {
        let (d, s) = (fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, d, bin(BinOp::PtrAdd, copy_local(a), int(dst, U64)));
        fb.assign(b, s, bin(BinOp::PtrAdd, copy_local(a), int(src, U64)));
        fb.push(
            b,
            Stmt::MemCopyDyn {
                dst: copy_local(d),
                src: copy_local(s),
                len: copy_local(len),
                overlapping,
            },
        );
    }
    for i in 0..4 {
        b = env.show(&mut fb, b, Rvalue::Use(copy_place(field(buf, i))), I64);
    }
    fb.ret(b, copy_place(field(buf, 1)));
    env.pb.add(fb.finish());
    let inputs = [(0, 0), (5, 0x41), (15, 0xFF), (-1, 7)]
        .iter()
        .map(|&(n, b)| vec![arg(n), arg(b)])
        .collect();
    env.case("buffer_shuffle", "shuffle", inputs)
}
