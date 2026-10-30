//! [`super::check`] with real sema (#1872): every variable a refused `with` callback reaches is
//! recorded by the first check, so they all move into `main` in one extra check.

#[allow(dead_code)]
#[path = "../tests/common/programs.rs"]
mod programs;

use crate::{CheckOptions, SEMA_STACK_BUDGET, SEMA_STACK_BYTES};

/// What checking a program as `check_with` does gives.
struct Run {
    /// How many times sema checked it.
    checks: usize,
    /// The variables the first check refused, sorted.
    first_refused: Vec<String>,
    /// The error messages of the last check.
    errors: Vec<String>,
}

/// Check `src` as `check_with` does, on a thread with sema's stack.
fn run(src: &str) -> Run {
    let l = programs::load_src(src);
    let (modules, root) = (&l.modules, l.root);
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .stack_size(SEMA_STACK_BYTES)
            .spawn_scoped(s, || {
                let (mut checks, mut first) = (0, None);
                let (_, diags) = super::check(modules, root, |m| {
                    checks += 1;
                    let opts = CheckOptions::default();
                    let out = crate::check_timed(m, root, opts, &[], SEMA_STACK_BUDGET);
                    first.get_or_insert_with(|| out.1.vars.clone());
                    out
                });
                let mut first_refused: Vec<String> = first.unwrap_or_default();
                first_refused.sort();
                first_refused.dedup();
                let errors = diags.iter().filter(|d| d.is_error());
                Run {
                    checks,
                    first_refused,
                    errors: errors.map(|d| d.message.clone()).collect(),
                }
            });
        spawned.expect("sema thread").join().expect("sema thread")
    })
}

/// A `with` callback that reads five `let`s that closures assign (the parser keeps them at
/// module level for the closures): the first check refuses all five, and the second, with them
/// in `main`, passes.
#[test]
fn a_with_callback_reading_many_moves_them_in_one_extra_check() {
    let mut src = String::from("const m = shared(new Mutex<number>(0));\n");
    let mut sum = vec![];
    for i in 0..5 {
        src += &format!("let v{i} = {i};\nconst b{i} = () => {{ v{i}++; }};\nb{i}();\n");
        sum.push(format!("v{i}"));
    }
    src += &format!("m.with((x) => {{ x += {}; }});\n", sum.join(" + "));
    src += "console.log(m.with((x) => x));\n";
    let r = run(&src);
    assert_eq!(r.errors, Vec::<String>::new());
    let all = ["v0", "v1", "v2", "v3", "v4"].map(String::from).to_vec();
    assert_eq!((r.checks, r.first_refused), (2, all));
}

/// A `with` callback that calls a closure using such variables: the check can't follow a call
/// through a function value, so the variables the closure's initializer names move.
#[test]
fn a_with_callback_calling_a_closure_moves_what_the_closure_uses() {
    let src = "const m = shared(new Mutex<number>(0));
        let c1 = 1;
        let c2 = 2;
        const b = () => { c1++; c2++; };
        const add = () => c1 + c2;
        b();
        m.with((v) => { v += add(); });
        console.log(m.with((v) => v));";
    let r = run(src);
    assert_eq!(r.errors, Vec::<String>::new());
    assert_eq!(r.checks, 2);
}
