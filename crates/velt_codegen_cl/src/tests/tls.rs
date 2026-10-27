//! The `velt_tls_*` externs on the operating system's thread keys (crate::tls), JIT-compiled for
//! the host (`cranelift-jit` has no thread-local storage): every thread starts with the starting
//! value, and a thread's `set` changes only its own value. Thread-local storage itself (objects)
//! is run by the `std/thread_local` golden.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::atomic::word;
use velt_vir::tls::TlsCall;
use velt_vir::vir::{Callee, Program, Rvalue, Terminator, Ty};

use super::{copy_local, FuncBuilder, ProgramBuilder};

/// (bits, starting word) per constant: zero, high bits of each width, a negative 32-bit value.
const VALUES: [(u32, u64); 4] = [(8, 0x80), (32, 0xFFFF_FFFD), (64, 0), (64, u64::MAX)];

/// Per constant `i`: `get_<i>() -> U64` and `set_<i>(v: U64) -> U64` (giving 0).
fn program() -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for (i, &(bits, init)) in VALUES.iter().enumerate() {
        let id = i as u32 + 10;
        let w = word(bits);
        let get = TlsCall::Get { id, bits, init };
        let ext = pb.ext(&get.symbol(), &[], w, false);
        let mut fb = FuncBuilder::internal(&format!("get_{i}"), &[], Ty::U64);
        let (r, out) = (fb.local(w), fb.local(Ty::U64));
        let bb = fb.block();
        let bb = fb.call(
            bb,
            Callee::Extern(ext),
            vec![],
            Some(velt_vir::vir::Place::local(r)),
        );
        fb.assign(bb, out, Rvalue::Cast(copy_local(r), Ty::U64));
        fb.term(bb, Terminator::Return(copy_local(out)));
        pb.add(fb.finish());

        let set = TlsCall::Set { id, bits, init };
        let ext = pb.ext(&set.symbol(), &[w], Ty::Unit, false);
        let mut fb = FuncBuilder::internal(&format!("set_{i}"), &[Ty::U64], Ty::U64);
        let v = fb.local(w);
        let bb = fb.block();
        fb.assign(bb, v, Rvalue::Cast(copy_local(fb.param(0)), w));
        let bb = fb.call(bb, Callee::Extern(ext), vec![copy_local(v)], None);
        fb.term(bb, Terminator::Return(super::int(0, Ty::U64)));
        pb.add(fb.finish());
    }
    pb.p
}

#[test]
fn thread_keys_keep_one_value_per_thread() {
    let isa = crate::isa::make_isa("", None, false, true).expect("ISA");
    let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    #[cfg(unix)]
    jb.symbol_lookup_fn(Box::new(crate::c_symbols::lookup));
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let program = program();
    let built = crate::module::build_module(&mut module, &program, &crate::module::Naming::Program)
        .expect("build");
    module.finalize_definitions().expect("finalize");
    type Get = extern "C" fn() -> u64;
    type Set = extern "C" fn(u64) -> u64;
    let code: Vec<usize> = (0..VALUES.len() * 2)
        .map(|i| module.get_finalized_function(built.funcs[i].expect("defined")) as usize)
        .collect();
    let get = |i: usize| -> Get { unsafe { std::mem::transmute(code[2 * i]) } };
    let set = |i: usize| -> Set { unsafe { std::mem::transmute(code[2 * i + 1]) } };
    for (i, &(bits, init)) in VALUES.iter().enumerate() {
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1 << bits) - 1
        };
        assert_eq!(get(i)(), init, "constant {i} starts as its starting value");
        set(i)(0x5A5A_5A5A_5A5A_5A5A);
        let mine = 0x5A5A_5A5A_5A5A_5A5A & mask;
        assert_eq!(get(i)(), mine, "constant {i} after a set");
        // Another thread starts with the starting value, and its set leaves this one's.
        let (g, s) = (code[2 * i], code[2 * i + 1]);
        let other = std::thread::spawn(move || {
            let (g, s) = unsafe {
                (
                    std::mem::transmute::<usize, Get>(g),
                    std::mem::transmute::<usize, Set>(s),
                )
            };
            let first = g();
            s(3);
            (first, g())
        })
        .join()
        .expect("thread");
        assert_eq!(other, (init, 3), "constant {i} on another thread");
        assert_eq!(get(i)(), mine, "constant {i} after another thread's set");
    }
    unsafe { module.free_memory() };
}
