//! X25519 key generation's masks stay masks in optimized code (`pick` in
//! `std/crypto/x25519_base.velt`). After the masked scan of a table, `pick` selects the identity
//! for a zero digit and negates for a negative one, through masks of the secret scalar's digit.
//! Inlined, such a mask is one comparison to LLVM, and a review's builder compiled the identity's
//! select to `test; js`: a branch on the digit, which no test run sees.
//!
//! So `pick` takes each of those masks from a function of its own, kept out of line
//! (`identityMask`, `signMask`), and this reads the IR after clang's optimizer, on x86-64 and
//! arm64: both are still functions of their own and something calls each, and no select in the X25519 code chooses between addresses or has a
//! value loaded from memory as an arm (as `p256_masks.rs` checks for P-256). What the backend
//! makes of the IR is not seen here; on x86-64 that is memcheck's to check, with the scalar marked
//! undefined.

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
            let arms = values(op);
            let pointers = op.contains(", ptr ");
            let memory = arms.iter().skip(1).any(|v| loaded.contains(v));
            (pointers || memory).then(|| l.to_string())
        })
        .collect()
}

#[test]
fn x25519_key_generation_masks_stay_masks() {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/x25519_masks/keygen.velt");
    let mut checked = 0;
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let Some(ir) = optimized(&clang, release_ir(&input, target), target) else {
            eprintln!("note: this clang has no {target}; skipping it");
            continue;
        };
        let functions: Vec<&str> = ir.split("\ndefine ").skip(1).collect();
        let x25519: Vec<&str> = functions
            .iter()
            .copied()
            .filter(|f| f.lines().next().is_some_and(|l| l.contains("x25519")))
            .collect();
        // Mangled names: if these are gone, the code moved and this test looks at nothing.
        let header = |f: &&str| f.lines().next().unwrap_or("").to_string();
        // Each mask of `pick`'s selects has a function of its own, so a regression of either
        // one (computed inline again) is seen, not only of both.
        for mask in ["identityMask", "signMask"] {
            assert!(
                x25519
                    .iter()
                    .any(|f| header(f).contains("x25519__base") && header(f).contains(mask)),
                "{target}: `{mask}` of x25519_base.velt is not a function of its own, so the \
                 caller sees that its mask is a comparison"
            );
            // The base multiplication may be inlined into its callers, up to the program's own
            // code; whichever function holds `pick`'s selects must take the mask from the call.
            let called = functions
                .iter()
                .filter(|f| !header(f).contains(mask))
                .any(|f| f.lines().any(|l| l.contains("call ") && l.contains(mask)));
            assert!(
                called,
                "{target}: no function calls `{mask}`, so `pick` computes that mask where LLVM \
                 can see through it; the X25519 functions are:\n{}",
                x25519.iter().map(header).collect::<Vec<_>>().join("\n")
            );
        }
        let found: Vec<String> = x25519.iter().flat_map(|f| selects_of_memory(f)).collect();
        assert!(
            found.is_empty(),
            "{target}: a mask became a select of memory or of addresses:\n{}",
            found.join("\n")
        );
        checked += 1;
    }
    assert!(checked > 0, "clang has none of the targets");
}
