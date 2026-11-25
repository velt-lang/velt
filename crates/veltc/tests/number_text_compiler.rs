//! The compiler's number text (`velt_syntax::js_number`, on `velt_text::fmt`, which a float
//! literal's text and sema's literal types use) is std's: a program prints `String(x)` for
//! hundreds of thousands of doubles through std's shortest printer (velt/lang/number/format,
//! whose output is Node's, golden `lang/number_text_shortest`), and every line must be what
//! `js_number` gives for the same bits (#3581). The doubles: -0, NaN and the infinities, the
//! subnormals, every power of two and of ten with their neighbours, the layout's edges at 1e-7,
//! 1e-6 and 1e21, 2^53 and its neighbours, an exact tie between two shortest candidates, and a
//! large random sample of bit patterns, each with its negative.

use no_window::command;

mod no_window;
mod test_dir;

const PROGRAM: &str = r#"
// Every double below, and its negative, as `bits<TAB>String(x)`.
let seed: u64 = 0x9e3779b97f4a7c15;

function next(): u64 {
  seed = seed ^ (seed << 13);
  seed = seed ^ (seed >> 7);
  seed = seed ^ (seed << 17);
  return seed;
}

function show(v: f64) {
  console.log(`${v.toBits()}\t${String(v)}`);
  const n = -v;
  console.log(`${n.toBits()}\t${String(n)}`);
}

// `v` and its neighbours `k` steps away on both sides.
function around(v: f64, k: u64) {
  const b = v.toBits();
  for (let i: u64 = 0; i <= k; i++) {
    show(f64.fromBits(b + i));
    if (b >= i) {
      show(f64.fromBits(b - i));
    }
  }
}

function main() {
  show(0);
  show(NaN);
  show(Infinity);
  // Subnormals: the smallest, then random ones.
  for (let b: u64 = 1; b <= 2000; b++) {
    show(f64.fromBits(b));
  }
  for (let i = 0; i < 2000; i++) {
    show(f64.fromBits(next() >> 12));
  }
  // Every power of two, the largest subnormal and the smallest normal.
  for (let e: u64 = 1; e < 2047; e++) {
    around(f64.fromBits(e << 52), 1);
  }
  around(f64.fromBits(0x000fffffffffffff), 2);
  // The powers of ten.
  for (let e = -330; e <= 310; e++) {
    around(parseFloat(`1e${e}`), 2);
  }
  // Where the layout changes: 1e-7 and 1e21, and 2^53 with its neighbours.
  around(1e-7, 50);
  around(1e-6, 50);
  around(1e21, 50);
  around(9007199254740992, 3);
  // Exact ties between two shortest candidates (the even one wins).
  show(1658206780088562.25);
  // A large random sample of bit patterns.
  for (let i = 0; i < 300000; i++) {
    show(f64.fromBits(next()));
  }
}
"#;

#[test]
fn compiler_number_text_is_std_shortest_printer() {
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("numbers.velt");
    std::fs::write(&file, PROGRAM).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "numbers.exe"
    } else {
        "numbers"
    });
    let built = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg("--release")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = command(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8(run.stdout).unwrap();
    let mut checked = 0;
    for line in out.lines() {
        let (bits, std_text) = line.split_once('\t').expect("bits<TAB>text");
        let v = f64::from_bits(bits.parse().unwrap());
        assert_eq!(
            velt_syntax::js_number(v),
            std_text,
            "bits {:#x} ({v:e})",
            v.to_bits()
        );
        checked += 1;
    }
    assert!(checked > 600_000, "only {checked} values");
}
