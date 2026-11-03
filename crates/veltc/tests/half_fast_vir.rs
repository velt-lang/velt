//! The f16/bf16 fast path (velt_vir `lower/half.rs`, `half_fast`) fires where it should and only
//! there. Its results are bit-identical to the exact f64 path by construction, so the goldens
//! pass either way; this test reads the VIR to see which path each function took.

mod no_window;
mod test_dir;

const SRC: &str = "function sqrtF16(a: f16): f16 {
  return Math.sqrt(a) as f16;
}
function sqrtBf16(a: bf16): bf16 {
  return Math.sqrt(a) as bf16;
}
function sqrtLiteral(): f16 {
  return Math.sqrt(2) as f16;
}
function mulF16(a: f16, b: f16): f16 {
  return (a * b) as f16;
}
function sqrtOfProduct(a: f16): f16 {
  return Math.sqrt(a * 1.0001) as f16;
}
function sqrtMixed(a: bf16): f16 {
  return Math.sqrt(a) as f16;
}
function sqrtScaled(a: f16): f16 {
  return (Math.sqrt(a) * 0.5) as f16;
}
console.log(sqrtF16(2 as f16), sqrtBf16(2 as bf16), sqrtLiteral(), mulF16(3 as f16, 5 as f16));
console.log(sqrtOfProduct(2 as f16), sqrtMixed(2 as bf16), sqrtScaled(2 as f16));
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The VIR of the function `name` (mangled `_V7sqrtF16`), from its `fn#` header to its `}`.
fn body<'a>(vir: &'a str, name: &str) -> &'a str {
    let head = format!(" _V{}{name}(", name.len());
    let start = vir
        .lines()
        .filter(|l| l.starts_with("fn#") && l.contains(&head))
        .map(|l| l.as_ptr() as usize - vir.as_ptr() as usize)
        .next()
        .unwrap_or_else(|| panic!("no function {name} in:\n{vir}"));
    let rest = &vir[start..];
    &rest[..rest.find("\n}\n").unwrap_or(rest.len())]
}

#[test]
fn the_fast_path_fires_where_it_should() {
    let vir = vir(SRC);
    for name in ["sqrtF16", "sqrtBf16", "sqrtLiteral"] {
        let b = body(&vir, name);
        assert!(b.contains("math.sqrt_f32("), "{name}: no f32 sqrt\n{b}");
        assert!(
            !b.contains("math.sqrt("),
            "{name}: an f64 sqrt is left\n{b}"
        );
        assert!(!b.contains("f64"), "{name}: computes in f64\n{b}");
    }
    let b = body(&vir, "mulF16");
    assert!(!b.contains("f64"), "mulF16: computes in f64\n{b}");
}

#[test]
fn everything_else_stays_on_the_exact_path() {
    let vir = vir(SRC);
    for name in ["sqrtOfProduct", "sqrtMixed", "sqrtScaled"] {
        let b = body(&vir, name);
        assert!(b.contains("math.sqrt("), "{name}: no f64 sqrt\n{b}");
        assert!(
            !b.contains("math.sqrt_f32("),
            "{name}: took the fast path\n{b}"
        );
    }
}
