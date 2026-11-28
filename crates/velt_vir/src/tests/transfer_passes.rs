//! A value transferred as a whole is a transfer root from the first lowering pass on, also when
//! that pass does not count its parts yet (`Cx::root_transfers`): the types a transfer copies
//! (boxing/transfers.rs) are then complete after the pass that finds the counted types, and the
//! lowering takes no extra pass of the whole program to find them (#3080: std's regex engine and
//! JSON tape made every program that used them take a third pass).

use crate::lower::PASSES;
use crate::{lower_seeded, LowerOptions};

use super::std_src::check_src;

/// The passes the lowering of `src` took, its program verified.
fn passes(src: &str) -> usize {
    let hir = check_src(src);
    let before = PASSES.with(|n| n.get());
    let (program, _) = lower_seeded(&hir, &LowerOptions::default()).expect("lowered");
    if let Err(errs) = crate::verify(&program) {
        panic!("verify failed:\n{}", errs.join("\n"));
    }
    PASSES.with(|n| n.get()) - before
}

/// `Node` is counted (shared through a closure and a second binding), which the first pass
/// finds; `Box` holds one and goes into a `shared(...)` as a whole.
const SHARED_ROOT: &str = "class Node { v: number = 0; }
class Box { n: Node; constructor(n: Node) { this.n = n; } }
function main() {
  const a = new Node();
  const b = a;
  const bump = () => { b.v += 1; };
  bump();
  console.log(a.v);
  const s = shared(new Box(new Node()));
  console.log(s.n.v);
}
";

#[test]
fn a_shared_value_of_a_type_counted_later_takes_two_passes() {
    assert_eq!(passes(SHARED_ROOT), 2, "one pass finds the counted types");
}

/// std's regex engine keeps what its automatons worked out in an `Atomic` (a store transfers):
/// a program that tests one regex.
const REGEX_ROOT: &str = "function main() {
  const s = \"123\";
  if (/^[0-9]+$/.test(s)) {
    console.log(\"digits\");
  }
}
";

#[test]
fn a_program_that_tests_a_regex_takes_two_passes() {
    assert_eq!(passes(REGEX_ROOT), 2, "one pass finds the counted types");
}
