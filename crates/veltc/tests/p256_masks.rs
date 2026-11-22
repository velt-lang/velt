//! P-256's masks stay masks in optimized code (`isZero` and `blend`, `std/crypto/field_p256.velt`;
//! `entryMasks`, the masks of the base table's scan in `std/crypto/p256_base_select.velt`).
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

use std::collections::{HashMap, HashSet};
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

/// One function's definitions: each `%name` and its right-hand side, without the metadata.
fn definitions(function: &str) -> HashMap<&str, &str> {
    function
        .lines()
        .filter_map(|l| {
            let (name, rhs) = l.trim().split_once(" = ")?;
            let rhs = rhs.split(", !").next().unwrap_or(rhs);
            Some((name.strip_prefix('%')?, rhs))
        })
        .collect()
}

/// Whether a `getelementptr`'s indices are all constants: a field at a fixed offset (an array's
/// length, its data pointer), not an element at a computed index.
fn constant_offset(gep: &str) -> bool {
    gep.split(", ").skip(2).all(|index| !index.contains('%'))
}

/// The branches, selects, switches and addresses in `function` that depend on a secret, which is
/// any value that comes from an element loaded at a computed index (the scalar's bytes, the
/// masks, the table's entries), from a call to one of `secret_calls`, or from the parameters at
/// `secret_args`. A public loop counter or a length (a field at a fixed offset) is not one, nor is
/// a comparison kept as a value (a borrow, `(a < b) as u64`): only a condition that steers control
/// or an address that picks memory is found.
fn secret_dependences(function: &str, secret_args: &[usize], secret_calls: &[&str]) -> Vec<String> {
    let head = function.lines().next().unwrap_or("");
    let params = values(head);
    let defs = definitions(function);
    let mut secret: HashSet<&str> = secret_args
        .iter()
        .filter_map(|&i| params.get(i).copied())
        .collect();
    loop {
        let before = secret.len();
        for (&name, &rhs) in &defs {
            if secret.contains(name) {
                continue;
            }
            let operands = values(rhs);
            let from_secret = if rhs.starts_with("load ") {
                operands.first().is_some_and(|p| {
                    secret.contains(p)
                        || defs
                            .get(p)
                            .is_some_and(|g| g.starts_with("getelementptr") && !constant_offset(g))
                })
            } else if rhs.contains("call ") {
                secret_calls.iter().any(|c| rhs.contains(c))
                    || operands.iter().any(|o| secret.contains(o)) && rhs.contains("call i")
            } else {
                operands.iter().any(|o| secret.contains(o))
            };
            if from_secret {
                secret.insert(name);
            }
        }
        if secret.len() == before {
            break;
        }
    }
    let mut found = vec![];
    for line in function.lines().map(str::trim) {
        let condition = if let Some(rest) = line.strip_prefix("br i1 ") {
            values(rest).first().copied()
        } else if let Some(rest) = line.strip_prefix("switch ") {
            values(rest).first().copied()
        } else {
            line.split_once(" = ")
                .and_then(|(_, rhs)| rhs.strip_prefix("select i1 "))
                .and_then(|rest| values(rest).first().copied())
        };
        if condition.is_some_and(|c| secret.contains(c)) {
            found.push(line.to_string());
        }
        if let Some((_, rhs)) = line.split_once(" = ") {
            if rhs.starts_with("getelementptr") {
                let rhs = rhs.split(", !").next().unwrap_or(rhs);
                if values(rhs).iter().skip(1).any(|i| secret.contains(i)) {
                    found.push(line.to_string());
                }
            }
        }
    }
    found
}

#[test]
fn a_branch_on_a_secret_is_found() {
    // An element loaded at a computed index, compared, steering a branch: found.
    let element = "define void @f(ptr %a, i64 %j) {\n\
                   %p = getelementptr inbounds i64, ptr %a, i64 %j\n\
                   %m = load i64, ptr %p, align 8\n\
                   %z = icmp eq i64 %m, 0\n\
                   br i1 %z, label %skip, label %take\n";
    assert_eq!(secret_dependences(element, &[], &[]).len(), 1);
    // The same through a select, and an address computed from the element.
    let select = "define void @f(ptr %a, i64 %j) {\n\
                  %p = getelementptr inbounds i64, ptr %a, i64 %j\n\
                  %m = load i64, ptr %p, align 8\n\
                  %z = icmp eq i64 %m, 0\n\
                  %s = select i1 %z, i64 1, i64 2\n\
                  %q = getelementptr inbounds i64, ptr %a, i64 %m\n";
    assert_eq!(secret_dependences(select, &[], &[]).len(), 2);
    // A secret parameter, and a secret call's result.
    let param = "define void @f(ptr %a, i64 %d) {\n\
                 %z = icmp eq i64 %d, 0\n\
                 br i1 %z, label %x, label %y\n";
    assert_eq!(secret_dependences(param, &[1], &[]).len(), 1);
    assert!(secret_dependences(param, &[], &[]).is_empty());
    let call = "define void @f(i64 %w) {\n\
                %n = call i64 @_V3N8signMask(i64 %w)\n\
                %z = icmp ne i64 %n, 0\n\
                br i1 %z, label %x, label %y\n";
    assert_eq!(secret_dependences(call, &[], &["N8signMask("]).len(), 1);
    // Public: a loop counter against a length at a fixed offset, and a borrow kept as a value.
    let public = "define void @f(ptr %a, i64 %j) {\n\
                  %lp = getelementptr inbounds i8, ptr %a, i64 8\n\
                  %len = load i64, ptr %lp, align 8\n\
                  %c = icmp ult i64 %j, %len\n\
                  br i1 %c, label %in, label %out\n\
                  %p = getelementptr inbounds i64, ptr %a, i64 %j\n\
                  %m = load i64, ptr %p, align 8\n\
                  %b = icmp ult i64 %m, 5\n\
                  %w = zext i1 %b to i64\n";
    assert!(secret_dependences(public, &[], &[]).is_empty());
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
        assert!(
            named("N12baseMultiply"),
            "{target}: `baseMultiply` is not a function of its own, so its code was not checked"
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
        // Nothing in the fixed-base multiplication steers control or picks an address by the
        // secret scalar: its bytes, the window's digit, the masks or the table entries they keep
        // (a scan that skips an entry by its mask, say, which no name check sees).
        let mut secret_found = vec![];
        for f in &p256 {
            let head = f.lines().next().unwrap_or("");
            let digit_last = head.contains("N10selectBase") || head.contains("N10entryMasks");
            if head.contains("N12baseMultiply") || digit_last {
                let last = values(head).len().saturating_sub(1);
                let args: &[usize] = if digit_last { &[last] } else { &[] };
                secret_found.extend(secret_dependences(f, args, &["N8signMask("]));
            }
        }
        assert!(
            secret_found.is_empty(),
            "{target}: the fixed-base multiplication branches on or addresses by the secret:\n{}",
            secret_found.join("\n")
        );
        // `baseMultiply`'s scan of the base table: inlined, the masks were a comparison of the
        // secret digit, and LLVM made the scan a branch that loaded only the selected entry.
        assert!(
            named("N10entryMasks"),
            "{target}: `entryMasks` was inlined, so the scan of the base table sees its masks are a comparison"
        );
        assert!(
            p256.iter().any(|f| f.contains("N10entryMasks(")),
            "{target}: nothing calls `entryMasks`"
        );
        // The negation of a negative digit's entry: inlined, its mask is one bit of the secret
        // window.
        assert!(
            named("N8signMask"),
            "{target}: `signMask` was inlined, so the negation sees its mask is a bit of the window"
        );
        assert!(
            p256.iter().any(|f| f.contains("N8signMask(")),
            "{target}: nothing calls `signMask`"
        );
        checked += 1;
    }
    assert!(checked > 0, "clang has none of the targets");
}
