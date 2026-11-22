//! Every home of an `@align(n)` struct's value keeps its alignment (#3222 step 6,
//! docs/internals/design/package-machine-code.md §5): a generator's state held across a
//! `yield` (the object moves in its block, `gen_object.rs` `gen_lead`), a channel's slots (std's
//! memory, copied in and out through an 8-aligned view, `slot.rs` `raw_view`), a spawned task's
//! state and result (`sched_make.velt` rounds the task's root up) and a module variable
//! (`module_blocks.velt` rounds the block up). Each used to be a compiler panic or a run-time
//! one. The program runs in debug and release builds, and in a release build for `x86-64-v3`,
//! whose aligned vector moves fault on a misaligned block.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

const PROGRAM: &str = r#"import { align } from "velt/compiler";
import { channel } from "velt/sync";

@align(32)
struct L {
  a: u64;
  b: u64;
  c: u64;
  d: u64;
}

@align(64)
struct Counter {
  n: u64;
}

let counter: Counter = { n: 1 };

function mkL(n: u64): L {
  return { a: n, b: n * 2, c: n * 3, d: n * 4 };
}

function sum(l: L): u64 {
  return l.a + l.b + l.c + l.d;
}

function* gen(n: u64): Generator<u64> {
  const l: L = mkL(n);
  yield l.a;
  yield sum(l);
}

function makeGen(n: u64): Generator<u64> {
  return gen(n);
}

async function work(l: L): Promise<u64> {
  await Promise.resolve(0);
  return sum(l);
}

async function make(n: u64): Promise<L> {
  await Promise.resolve(0);
  return mkL(n);
}

async function main() {
  let generated: u64 = 0;
  for (let i: u64 = 0; i < 100; i++) {
    for (const x of makeGen(i)) {
      generated += x;
    }
  }
  const ch = channel<L>(4);
  let sent: u64 = 0;
  for (let round: u64 = 0; round < 25; round++) {
    for (let i: u64 = 0; i < 4; i++) {
      await ch.send(mkL(i + round));
    }
    for (let i: u64 = 0; i < 4; i++) {
      const l = await ch.receive();
      if (l !== null) {
        sent += sum(l);
      }
    }
  }
  let spawned: u64 = 0;
  for (let i: u64 = 0; i < 50; i++) {
    spawned += await spawn(work(mkL(i)));
    const l = await spawn(make(i));
    spawned += l.d;
  }
  for (let i = 0; i < 10; i++) {
    counter = { n: counter.n * 2 };
  }
  console.log(generated, sent, spawned, counter.n);
}
"#;

/// generated: 11 · (0 + … + 99); sent: 10 · Σ(i + round) over 25 rounds of i = 0..3; spawned:
/// 14 · (0 + … + 49); the counter: 2^10.
const EXPECTED: &str = "54450 13500 17150 1024\n";

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_BUILD_CACHE", "off")
        .output()
        .unwrap()
}

/// Whether this machine runs an `x86-64-v3` build (AVX2, BMI2, FMA, …).
fn runs_x86_64_v3() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2")
            && std::is_x86_feature_detected!("bmi2")
            && std::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[test]
fn aligned_values_keep_their_alignment_in_generators_channels_tasks_and_modules() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let mut builds: Vec<Vec<&str>> = vec![vec![], vec!["--release"]];
    if runs_x86_64_v3() {
        builds.push(vec!["--release", "--cpu", "x86-64-v3"]);
    }
    for extra in builds {
        let mut args = vec!["run", "main.velt"];
        args.extend(&extra);
        let o = velt(dir.path(), &args);
        assert!(
            o.status.success(),
            "{extra:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&o.stdout), EXPECTED, "{extra:?}");
    }
}

/// The two homes the compiler builds: a generator's object is allocated aligned to its state
/// and starts past a lead (`gen_lead`), so the state at its fixed offset is aligned; a channel
/// slot is written and read through an 8-aligned view of the value (`raw_view`), the value
/// itself staying in an aligned temporary. (The run above can't tell an aligned block from a
/// misaligned one: Velt code takes no addresses, and its moves are the unaligned forms. The
/// task's and module block's rounding: std/lang/hooks/aligned_blocks.test.velt.)
#[test]
fn a_generator_object_and_a_channel_slot_are_laid_out_for_the_alignment() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let o = velt(dir.path(), &["build", "main.velt", "--emit", "vir"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let vir = String::from_utf8_lossy(&o.stdout).to_string();
    // The object's block: aligned to 32 (the state's alignment), freed the same way.
    for call in ["velt_rt_alloc(", "velt_rt_free("] {
        assert!(
            (vir.lines()).any(|l| l.contains(call) && l.contains(", 32_u64)")),
            "no `{call}…, 32)` for the generator object in:\n{vir}"
        );
    }
    // The slot's view: `L`'s 32 bytes as words, aligned to 8.
    assert!(
        (vir.lines()).any(|l| l.starts_with("agg#") && l.contains(" L bytes size=32 align=8 ")),
        "no 8-aligned view of `L` in:\n{vir}"
    );
}
