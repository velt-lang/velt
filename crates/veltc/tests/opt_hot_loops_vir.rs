//! Calls a release build keeps out of hot loops, checked on the optimized VIR: `Math.min` /
//! `Math.max` with plain arguments build no rest array (#1570), and module-level `let`s ask the
//! runtime for the module state once per function call (#1571). The goldens
//! `std/math_min_max_args.vlt` and `lang/module_let_loops.vlt` check the results.

use std::path::PathBuf;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The release VIR of the program `source`.
fn release_vir(source: &str) -> String {
    let opts = BuildOptions {
        input: PathBuf::from("hot_loops.vlt"),
        root_source: Some(source.to_string()),
        release: true,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

/// Each function of `vir` (by header) with its body's lines.
fn functions(vir: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in vir.lines() {
        if line.starts_with("fn#") {
            out.push((line, vec![]));
        } else if let Some((_, body)) = out.last_mut() {
            body.push(line);
        }
    }
    out
}

#[test]
fn math_min_and_max_with_plain_arguments_build_no_rest_array() {
    let vir = release_vir(
        "function smallest(xs: number[], cap: number): number {
           let m = Infinity;
           for (let i = 0; i < xs.length; i++) {
             m = Math.min(m, xs[i]!, cap);
           }
           return Math.max(m, -cap);
         }
         function main() {
           const xs: number[] = [];
           for (let i = 0; i < 10; i++) xs.push((i * 7) % 5);
           console.log(smallest(xs, 3));
         }",
    );
    // The prelude's `Math.min` / `Math.max` (which take the rest array) are not called at all.
    assert!(!vir.contains("MathM3min"), "{vir}");
    assert!(!vir.contains("MathM3max"), "{vir}");
}

#[test]
fn module_lets_in_loops_ask_for_the_module_state_once_per_call() {
    let vir = release_vir(
        "let seed = 42;
         let hits = 0;
         function next(): number {
           seed = (seed * 3877 + 29573) % 139968;
           return seed;
         }
         function main() {
           let s = 0;
           for (let i = 0; i < 1000; i++) {
             s += next() % 10;
             hits += 1;
           }
           console.log(s, seed, hits);
         }",
    );
    let lookups: Vec<_> = functions(&vir)
        .into_iter()
        .filter(|(_, body)| body.iter().any(|l| l.contains("velt_rt_module_state(")))
        .collect();
    assert!(!lookups.is_empty(), "{vir}");
    // Every function that asks keeps the answer for the rest of its call (`ms.checked`).
    for (f, body) in lookups {
        assert!(
            body.iter().any(|l| l.contains("// ms.checked")),
            "{f} asks at every access\n{vir}"
        );
    }
}
