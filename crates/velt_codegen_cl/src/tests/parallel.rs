//! Functions translate and compile on several threads (parallel.rs): the object must be the same
//! bytes whatever the number of threads, including the C library functions declared on first
//! use, and the line tables and unwind info of every function.

use super::*;
use crate::{emit_object_on, CodegenOptions};

/// A program of `n` functions of very different sizes (so threads finish out of order), some
/// taking float remainders (`fmod`, `fmodf`: declared when a function first needs them), with
/// source locations (line tables), all called from `velt_main`.
fn many_functions(n: u32) -> Program {
    let (mut pb, _rt) = ProgramBuilder::new();
    pb.p.files = vec!["many.velt".into()];
    let mut ids = Vec::new();
    for i in 0..n {
        let mut fb = FuncBuilder::internal(&format!("f{i}"), &[Ty::F64], Ty::F64);
        let x = fb.param(0);
        let mut b = fb.block();
        let acc = fb.local(Ty::F64);
        fb.assign(b, acc, Rvalue::Use(copy_local(x)));
        // Sizes from 1 to ~400 statements, the large ones early.
        let len = 1 + (n - i) * 7 % 400;
        for k in 0..len {
            let op = match (i % 5, k % 3) {
                (1, 0) => BinOp::Rem,
                _ => BinOp::Add,
            };
            let next = fb.local(Ty::F64);
            let rv = Rvalue::Binary(op, copy_local(acc), float(f64::from(k + 1), Ty::F64));
            fb.assign(b, next, rv);
            fb.assign(b, acc, Rvalue::Use(copy_local(next)));
            if k % 50 == 49 {
                let c = fb.block();
                fb.term(b, Terminator::Goto(c));
                b = c;
            }
        }
        if i % 7 == 3 {
            // An f32 remainder: `fmodf`.
            let a = fb.local(Ty::F32);
            fb.assign(b, a, Rvalue::Cast(copy_local(acc), Ty::F32));
            let r = fb.local(Ty::F32);
            let rv = Rvalue::Binary(BinOp::Rem, copy_local(a), float(3.0, Ty::F32));
            fb.assign(b, r, rv);
            fb.assign(b, acc, Rvalue::Cast(copy_local(r), Ty::F64));
        }
        fb.term(b, Terminator::Return(copy_local(acc)));
        let mut f = fb.finish();
        let stmts: Vec<usize> = f.blocks.iter().map(|b| b.stmts.len()).collect();
        f.locs = located(i, &stmts);
        ids.push(pb.add(f));
    }
    let (mut fb, mut cur) = main_fb();
    let v = fb.local(Ty::F64);
    for id in ids {
        cur = fb.call(
            cur,
            Callee::Func(id),
            vec![float(1.5, Ty::F64)],
            Some(Place::local(v)),
        );
    }
    pb.add(finish_main(fb, cur, 0));
    pb.p
}

/// Source locations for a function's statements and terminators: line `i + 1` of file 0.
fn located(i: u32, stmts: &[usize]) -> Vec<Vec<Option<SrcLoc>>> {
    let loc = SrcLoc {
        file: 0,
        line: i + 1,
        col: 1,
    };
    stmts.iter().map(|&n| vec![Some(loc); n + 1]).collect()
}

#[test]
fn objects_are_the_same_on_any_number_of_threads() {
    let program = many_functions(300);
    velt_vir::verify(&program).unwrap_or_else(|e| panic!("invalid VIR: {e:?}"));
    for target in [
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
    ] {
        let opts = CodegenOptions {
            target: target.into(),
            optimize: false,
            cpu: None,
        };
        let emit = |jobs| {
            emit_object_on(&program, &opts, None, &mut Vec::new(), jobs)
                .unwrap()
                .0
        };
        let one = emit(1);
        for jobs in [2, 3, 8, 32] {
            assert!(
                emit(jobs) == one,
                "{target}: {jobs} threads made another object"
            );
        }
        // Each C library function is imported once.
        let file = object::File::parse(&*one).unwrap();
        use object::{Object, ObjectSymbol};
        for lib in ["fmod", "fmodf"] {
            let name = super::objects::obj_name(file.format(), lib);
            let n = file.symbols().filter(|s| s.name() == Ok(&name)).count();
            assert_eq!(n, 1, "{target}: `{lib}` symbols");
        }
    }
}
