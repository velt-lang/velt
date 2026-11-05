//! A packed array literal of numbers (`hir::ExprKind::ConstArray`, #2421) lowers to exactly the
//! code of the `ArrayLit` it stands for: a module table to the same read-only data, a local
//! array to the same stores. Each program is checked twice, once with every array packed and once
//! with the first element of each in parentheses (`(0),1` for `0, 1`: the same length, so every
//! later position is the same), which keeps the parser from packing it.

use super::{lower_ok, run_src, std_src::check_src};

/// `[e0, e1, …]` of `n` elements, `f(i)` the text of element `i`, its first element in
/// parentheses (with the space after its comma dropped) when `paren`.
fn array(n: usize, paren: bool, f: impl Fn(usize) -> String) -> String {
    let elems: Vec<String> = (0..n).map(f).collect();
    match paren {
        true => format!("[({}),{}]", elems[0], elems[1..].join(", ")),
        false => format!("[{}]", elems.join(", ")),
    }
}

fn program(paren: bool) -> String {
    format!(
        "const T: u8[] = {};
const W: u64[] = {};
const F: f32[] = {};
const D: f64[] = {};
function sum(xs: i16[]): i16 {{ return xs[1] + xs[15]; }}
function main() {{
  const local: i32[] = {};
  let grow: f64[] = {};
  grow.push(0.5);
  console.log(T[3], T.length, W[17], F[1], D[2], local[5], grow.length, sum({}));
  for (const w of W) console.log(w);
  console.log(F);
}}",
        array(20, paren, |i| (i * 13 % 256).to_string()),
        array(18, paren, |i| format!("0x{:x}", (i as u64 + 1) << 40)),
        array(16, paren, |i| format!("{i}.25")),
        array(16, paren, |i| if i % 2 == 0 {
            format!("{i}")
        } else {
            format!("{i}e-3")
        }),
        array(24, paren, |i| (i * 1000).to_string()),
        array(17, paren, |i| format!("{i}.5")),
        array(16, paren, |i| (i * 2).to_string()),
    )
}

#[test]
fn const_arrays_lower_as_the_array_literals_they_stand_for() {
    let packed = check_src(&program(false));
    let plain = check_src(&program(true));
    let count =
        |p: &velt_sema::hir::Program| format!("{:?}", p.defs).matches("ConstArray(").count();
    // Std's own tables are packed in both.
    assert_eq!(
        count(&packed),
        count(&plain) + 7,
        "every array of the program is packed"
    );
    let (a, b) = (lower_ok(&packed).to_string(), lower_ok(&plain).to_string());
    if a != b {
        let at = a
            .lines()
            .zip(b.lines())
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        let show = |s: &str| {
            s.lines()
                .skip(at.saturating_sub(3))
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!("VIR differs at line {at}:\n{}\n---\n{}", show(&a), show(&b));
    }
    // The tables are read-only data: their bytes as stored.
    assert!(a.contains("static#"), "{a}");
}

#[test]
fn const_arrays_run() {
    let packed = run_src(&program(false));
    assert!(
        packed
            .stdout
            .starts_with("39 20 19791209299968 1.25 2 5000 18 32\n"),
        "{}",
        packed.stdout
    );
    assert_eq!(packed.stdout, run_src(&program(true)).stdout);
}
