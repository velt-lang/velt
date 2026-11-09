//! `velt_eh_catches` (crate::eh) in the `velt dev` JIT: 0, since the JIT registers no LSDA and its
//! `velt_eh_try` is a plain call, so std never raises Velt's exception there (a raise with no
//! catch to claim it would reach the runtime's frames). Objects, where it is 1 on macOS and Linux,
//! are run by veltc's `unwinding` test.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir::{Callee, Place, Rvalue, Terminator, Ty};

use super::{copy_local, FuncBuilder, ProgramBuilder};

#[test]
fn the_jit_does_not_catch() {
    let (mut pb, _) = ProgramBuilder::new();
    let (params, ret) = velt_vir::eh::catches_signature();
    assert!(params.is_empty());
    let ext = pb.ext(velt_vir::eh::CATCHES, &[], ret, false);
    let mut fb = FuncBuilder::internal("catches", &[], Ty::U64);
    let (r, out) = (fb.local(ret), fb.local(Ty::U64));
    let bb = fb.block();
    let bb = fb.call(bb, Callee::Extern(ext), vec![], Some(Place::local(r)));
    fb.assign(bb, out, Rvalue::Cast(copy_local(r), Ty::U64));
    fb.term(bb, Terminator::Return(copy_local(out)));
    pb.add(fb.finish());
    let program = pb.p;

    let isa = crate::isa::make_isa("", None, false, true).expect("ISA");
    let jb = JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());
    let mut module = JITModule::new(jb);
    let built = crate::module::build_module(
        &mut module,
        &*isa,
        &program,
        &crate::module::Naming::Program,
    )
    .expect("build");
    module.finalize_definitions().expect("finalize");
    let code = module.get_finalized_function(built.funcs[0].expect("defined"));
    // SAFETY: `catches` was compiled with signature () -> U64 for the host.
    let catches: extern "C" fn() -> u64 = unsafe { std::mem::transmute(code) };
    assert_eq!(catches(), 0);
}
