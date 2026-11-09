//! The counted types carried from one lowering to the next (lower/boxing/seed.rs, `velt dev`):
//! a seed naming exactly the types a program counts makes its lowering one pass, with the same
//! program as without it, also for another version of the program with another type table.

use crate::lower::PASSES;
use crate::{lower_seeded, CountedSeed, LowerOptions};

use super::std_src::check_src;

/// Objects shared through a closure and a second binding: their classes are counted, which a
/// lowering without a seed finds in its first pass and builds again in a second one.
const V1: &str = "class Node { v: number = 0; next: Node | null = null; }
function main() {
  const a = new Node();
  const b = a;
  const bump = () => { b.v += 1; };
  bump();
  a.next = new Node();
  console.log(a.v, b.v);
}
";

/// [`V1`] after an edit: a new function in front of `main` and a changed string, so the
/// definitions and the type table are another program's.
const V2: &str = "class Node { v: number = 0; next: Node | null = null; }
function twice(n: number): number { return n * 2; }
function main() {
  const a = new Node();
  const b = a;
  const bump = () => { b.v += 1; };
  bump();
  a.next = new Node();
  console.log(\"edited\", twice(a.v), b.v);
}
";

/// The program (as text), its seed, and the passes its lowering took.
fn lower(src: &str, seed: Option<&CountedSeed>) -> (String, CountedSeed, usize) {
    let hir = check_src(src);
    let opts = LowerOptions {
        counted_seed: seed,
        ..Default::default()
    };
    let before = PASSES.with(|n| n.get());
    let (program, seed) = lower_seeded(&hir, &opts).expect("lowered");
    if let Err(errs) = crate::verify(&program) {
        panic!("verify failed:\n{}", errs.join("\n"));
    }
    let passes = PASSES.with(|n| n.get()) - before;
    (program.to_string(), seed, passes)
}

#[test]
fn a_seed_of_the_same_program_lowers_it_once_and_the_same() {
    let (cold, seed, cold_passes) = lower(V1, None);
    assert_ne!(seed, CountedSeed::default(), "the program counts types");
    assert!(
        cold_passes >= 2,
        "without a seed, a pass finds the counted types"
    );
    let (warm, again, warm_passes) = lower(V1, Some(&seed));
    assert_eq!(warm_passes, 1, "the seed's types are the program's");
    assert_eq!(again, seed);
    assert!(warm == cold, "the same program with and without the seed");
}

#[test]
fn a_seed_carries_over_to_an_edited_version() {
    let (_, seed, _) = lower(V1, None);
    let (cold, cold_seed, _) = lower(V2, None);
    let (warm, warm_seed, warm_passes) = lower(V2, Some(&seed));
    assert_eq!(warm_passes, 1, "the edit counts the same types");
    assert_eq!(warm_seed, cold_seed);
    assert!(
        warm == cold,
        "the same program with and without the old version's seed"
    );
}

/// [`V1`] after an edit that stops sharing `Node`: one binding, no closure.
const V3: &str = "class Node { v: number = 0; next: Node | null = null; }
function main() {
  const a = new Node();
  a.v += 1;
  console.log(a.v);
}
";

/// A seed naming a type the edited version no longer needs (the case of a `velt dev` reload
/// that removes a share): it stays counted, in one pass, and the program is valid.
#[test]
fn a_stale_seed_keeps_its_types_counted_in_one_pass() {
    let (_, seed, _) = lower(V1, None);
    assert!(seed.names("adt Node"), "v1 counts Node: {seed:?}");
    let (_, cold_seed, _) = lower(V3, None);
    assert!(
        !cold_seed.names("adt Node"),
        "v3 alone counts nothing of Node"
    );
    // `lower` verifies the program.
    let (_, warm_seed, warm_passes) = lower(V3, Some(&seed));
    assert_eq!(warm_passes, 1, "nothing more to find");
    assert!(
        warm_seed.names("adt Node"),
        "the stale seed keeps Node counted"
    );
}

/// An `RwLock` of a class a `WeakRef` points at: its `read` is exclusive (a weak-capable object
/// cannot be read beside others), a write lock.
const WEAK_V1: &str = "class Config { n: number = 1; }
function main() {
  const c = new Config();
  const w = new WeakRef(c);
  const lk = new RwLock(c);
  lk.read((x) => console.log(x.n));
  console.log(w.deref() !== undefined);
}
";

/// [`WEAK_V1`] after an edit that drops the `WeakRef`: `read` is shared again.
const WEAK_V2: &str = "class Config { n: number = 1; }
function main() {
  const c = new Config();
  const lk = new RwLock(c);
  lk.read((x) => console.log(x.n));
}
";

/// What decides an exclusive `read` is never seeded (lower/boxing/seed.rs): the old version's
/// weak class must not make the edited version's reads exclusive (readers waiting for each
/// other would deadlock where the cold build lets them both read).
#[test]
fn a_stale_seed_never_makes_a_read_exclusive() {
    let (v1, seed, _) = lower(WEAK_V1, None);
    assert!(v1.contains("velt_rt_rwlock_write"), "v1 reads exclusively");
    let (cold, _, _) = lower(WEAK_V2, None);
    assert!(cold.contains("velt_rt_rwlock_read") && !cold.contains("velt_rt_rwlock_write"));
    let (warm, _, _) = lower(WEAK_V2, Some(&seed));
    assert!(
        warm.contains("velt_rt_rwlock_read") && !warm.contains("velt_rt_rwlock_write"),
        "v2 with v1's seed reads beside others, as cold"
    );
}
