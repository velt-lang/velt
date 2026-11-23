//! Every home of an `@align(n)` struct's value keeps its alignment (#3222 step 6,
//! docs/internals/design/package-machine-code.md §5): a generator's state held across a
//! `yield` (the object moves in its block, `gen_object.rs` `gen_lead`), a channel's slots (std's
//! memory, copied in and out through an 8-aligned view, `slot.rs` `raw_view`), a spawned task's
//! state and result (`sched_make.velt` rounds the task's root up) and a module variable
//! (`module_blocks.velt` rounds the block up). Each used to be a compiler panic or a run-time
//! one. The program runs in debug and release builds, and in a release build for `x86-64-v3`;
//! that run checks the values and that nothing stops, not the alignment (Velt code takes no
//! addresses, and a misaligned block can go unnoticed), which the layout test below and
//! std/lang/hooks/aligned_blocks.test.velt check.

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
    // Shared through two references: a counted object, its count in the word before it.
    const g = gen(i);
    const h = g;
    for (const x of g) {
      generated += x;
    }
    if (!h.next().done) {
      generated += 1000000;
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

/// generated: 2 · 11 · (0 + … + 99); sent: 10 · Σ(i + round) over 25 rounds of i = 0..3; spawned:
/// 14 · (0 + … + 49); the counter: 2^10.
const EXPECTED: &str = "108900 13500 17150 1024\n";

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
    // Each generator object: its block aligned to the state's 32 bytes, the object `lead` bytes
    // into it, so that the state at the generator class's offset is aligned; a counted object's
    // count in the word before it, inside the lead.
    let placed = placed_generators(&vir);
    // One generator object is shared, which makes the program's generator objects counted.
    assert!(
        placed.iter().all(|p| p.counted),
        "an uncounted generator object in:\n{vir}"
    );
    assert!(
        (vir.lines()).any(|l| l.contains("velt_rt_free(") && l.contains(", 32_u64)")),
        "the generator object is not freed with its block's alignment in:\n{vir}"
    );
    // The slot's view: `L`'s 32 bytes as words, aligned to 8.
    assert!(
        (vir.lines()).any(|l| l.starts_with("agg#") && l.contains(" L bytes size=32 align=8 ")),
        "no 8-aligned view of `L` in:\n{vir}"
    );
}

/// Where a generator object sits in its block, from the VIR: the block's alignment, the
/// object's offset into it, the state's offset in the object, and whether its count word is in
/// front of it.
#[derive(Debug)]
struct Placement {
    align: i64,
    lead: i64,
    state_off: i64,
    counted: bool,
}

/// Every allocation aligned beyond 8 whose block is then written as a generator object
/// (`agg#K <name> generator … { …, agg#S@<state offset> }`): `_b = call … velt_rt_alloc(n, align)`,
/// `_o = ptradd _b, lead`, then `ptradd _o, -8` for a count, and `(*_x as agg#K)` stores.
fn generator_placements(vir: &str) -> Vec<Placement> {
    let gens: std::collections::HashMap<String, i64> = (vir.lines())
        .filter(|l| l.starts_with("agg#") && l.contains(" generator size="))
        .filter_map(|l| {
            let id = l.split_whitespace().next()?.to_string();
            let last = l.trim_end_matches(" }").rsplit(", ").next()?;
            let off = last.rsplit('@').next()?.parse().ok()?;
            Some((id, off))
        })
        .collect();
    let lines: Vec<&str> = vir.lines().collect();
    let mut out = vec![];
    for (i, l) in lines.iter().enumerate() {
        let Some(args) = l.split("velt_rt_alloc(").nth(1) else {
            continue;
        };
        let Some(align) = (args.split(", ").nth(1))
            .and_then(|a| a.split("_u64").next())
            .and_then(|a| a.parse::<i64>().ok())
        else {
            continue;
        };
        if align <= 8 {
            continue;
        }
        let block = l.trim().split(" = ").next().unwrap_or("");
        let after = &lines[i + 1..(i + 12).min(lines.len())];
        let lead = after.iter().find_map(|a| {
            let rest = a.trim().split(" = ptradd ").nth(1)?;
            let (from, by) = rest.split_once(", ")?;
            (from == block).then(|| by.trim_end_matches("_i64").parse::<i64>().ok())?
        });
        let state_off = after.iter().find_map(|a| {
            let k = a.split(" as agg#").nth(1)?.split(')').next()?;
            gens.get(&format!("agg#{k}")).copied()
        });
        if let (Some(lead), Some(state_off)) = (lead, state_off) {
            let counted = after.iter().any(|a| a.contains(", -8_i64"));
            out.push(Placement {
                align,
                lead,
                state_off,
                counted,
            });
        }
    }
    out
}

/// The generator objects of `vir` (at least one), each with its state aligned and, when counted,
/// its count inside the lead.
fn placed_generators(vir: &str) -> Vec<Placement> {
    let placed = generator_placements(vir);
    assert!(!placed.is_empty(), "no aligned generator object in:\n{vir}");
    for p in &placed {
        assert_eq!(
            (p.lead + p.state_off) % p.align,
            0,
            "{p:?}: the state is not aligned in:\n{vir}"
        );
        if p.counted {
            assert!(p.lead >= 8, "{p:?}: no room for the count in:\n{vir}");
        }
    }
    placed
}

/// A program that shares no generator object, so its objects are uncounted.
const UNCOUNTED: &str = r#"import { align } from "velt/compiler";

@align(32)
struct L {
  a: u64;
  b: u64;
  c: u64;
  d: u64;
}

function* steps(n: u64): Generator<u64> {
  const l: L = { a: n, b: n * 2, c: n * 3, d: n * 4 };
  yield l.a;
  yield l.d;
}

function main() {
  let total: u64 = 0;
  for (let i: u64 = 0; i < 300; i++) {
    const g = steps(i);
    const r = g.next();
    if (!r.done) {
      total += r.value;
    }
    const s = g.next();
    if (!s.done) {
      total += s.value;
    }
  }
  console.log(total);
}
"#;

/// An uncounted generator object: no count word, so its lead is only what aligns the state
/// (`gen_lead`'s other arm); the program prints 5 · (0 + … + 299).
#[test]
fn an_uncounted_generator_object_is_laid_out_for_the_alignment() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), UNCOUNTED).unwrap();
    let o = velt(dir.path(), &["build", "main.velt", "--emit", "vir"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let vir = String::from_utf8_lossy(&o.stdout).to_string();
    let placed = placed_generators(&vir);
    assert!(
        placed.iter().all(|p| !p.counted),
        "a counted generator object in:\n{vir}"
    );
    for release in [false, true] {
        let mut args = vec!["run", "main.velt"];
        if release {
            args.push("--release");
        }
        let o = velt(dir.path(), &args);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "224250\n",
            "release: {release}"
        );
    }
}
