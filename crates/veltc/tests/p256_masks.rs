//! P-256's masks stay masks in optimized code (`isZero` and `blend`, `std/crypto/field_p256.velt`).
//! Inlined, `isZero` is one comparison to LLVM, so `blend`'s `(a & m) | (b & ~m)` becomes a select
//! of two loads, and the backend makes that one load through a selected pointer: a memory address
//! that depends on whether a window of the secret scalar is zero. No test run sees the difference.
//!
//! So this reads the IR after clang's optimizer, on x86-64 and arm64: in the functions of the P-256
//! code, no `select` chooses between pointers or has a value loaded from memory as an arm. A select
//! of two computed values compiles to `cmov` and is left alone here; nothing yet keeps those from
//! becoming branches (#2392). Code generation after the IR is not seen here: the check for that,
//! on x86-64, is the memcheck command in `field_p256.velt`.

mod optimized_ir;

use std::collections::HashSet;
use std::path::Path;

use optimized_ir::{optimized, release_ir, values};

/// The selects in one function's IR that choose by memory or between addresses.
fn selects_of_memory(function: &str) -> Vec<String> {
    let lines: Vec<&str> = function.lines().map(str::trim).collect();
    let loaded: HashSet<&str> = lines
        .iter()
        .filter_map(|l| l.split_once(" = "))
        .filter(|(_, op)| op.starts_with("load "))
        .map(|(result, _)| result.trim_start_matches('%'))
        .collect();
    lines
        .iter()
        .filter_map(|l| {
            let (_, op) = l.split_once(" = ")?;
            let op = op.strip_prefix("select ")?;
            // `i1 %c, <type> %a, <type> %b`: the arms come after the condition's value.
            let arms = values(op);
            let pointers = op.contains(", ptr ");
            let memory = arms.iter().skip(1).any(|v| loaded.contains(v));
            (pointers || memory).then(|| l.to_string())
        })
        .collect()
}

#[test]
fn a_select_of_two_loads_is_found() {
    let shape = "%a = load i64, ptr %p, align 8\n\
                 %b = load i64, ptr %q, align 8\n\
                 %s = select i1 %c, i64 %a, i64 %b, !dbg !7\n";
    assert_eq!(selects_of_memory(shape).len(), 1);
    let one = "%a = load i64, ptr %p, align 8\n\
               %s = select i1 %c, i64 %a, i64 0\n";
    assert_eq!(selects_of_memory(one).len(), 1);
    let address = "%s = select i1 %c, ptr %p, ptr %q\n";
    assert_eq!(selects_of_memory(address).len(), 1);
    let values_only = "%a = add i64 %x, 1\n\
                       %s = select i1 %c, i64 %a, i64 %y\n";
    assert!(selects_of_memory(values_only).is_empty());
    // A loaded condition is another matter (a public length, say), not the shape here.
    let condition = "%c = load i1, ptr %p, align 1\n\
                     %s = select i1 %c, i64 %x, i64 %y\n";
    assert!(selects_of_memory(condition).is_empty());
}

#[test]
fn p256_masks_stay_masks() {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/p256_masks/ecdh.velt");
    let mut checked = 0;
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let Some(ir) = optimized(&clang, release_ir(&input, target), target) else {
            eprintln!("note: this clang has no {target}; skipping it");
            continue;
        };
        let p256: Vec<&str> = ir
            .split("\ndefine ")
            .skip(1)
            .filter(|f| f.lines().next().is_some_and(|l| l.contains("p256")))
            .collect();
        // Mangled names: if these are gone, the code moved and this test looks at nothing.
        let named = |name: &str| {
            p256.iter()
                .any(|f| f.lines().next().is_some_and(|l| l.contains(name)))
        };
        assert!(
            named("N8multiply"),
            "{target}: `multiply` is not a function of its own, so its code was not checked"
        );
        let found: Vec<String> = p256.iter().flat_map(|f| selects_of_memory(f)).collect();
        assert!(
            found.is_empty(),
            "{target}: a mask became a select of memory or of addresses:\n{}",
            found.join("\n")
        );
        assert!(
            named("M6isZero"),
            "{target}: `isZero` was inlined, so the caller sees its mask is a comparison"
        );
        checked += 1;
    }
    assert!(checked > 0, "clang has none of the targets");
}
