//! A kept check (`velt_sema::kept`, dev-warm-front-end.md step 3 "C2"): the checking context of a
//! check, kept with everything it borrows. It reads its modules through its borrows after the
//! check and the driver's handles are gone, and it takes over only the very modules it borrows.

mod common;

use common::programs::load_src;
use velt_sema::generated::GenInput;
use velt_sema::{check_generating_kept, CheckOptions, Checked, KeptCheck, PendingKept};

/// Check `src` asking for a kept check: the pending kept check, if any, and the modules checked.
fn pending(src: &str) -> (Option<PendingKept>, Vec<velt_sema::SourceModule>) {
    let l = load_src(src);
    let modules = l.modules;
    let opts = CheckOptions::default();
    let input = GenInput::default();
    let ((out, diags), pending) =
        check_generating_kept(&modules, l.root, opts, &input, &[], None, None);
    assert!(
        matches!(out, Some(Checked::Program(_))),
        "the program checks: {diags:?}"
    );
    (pending, modules)
}

/// The kept check of `src`, and what it should read: the paths and the item count of the
/// modules, taken before they were handed over.
fn kept(src: &str) -> (KeptCheck, Vec<String>, usize) {
    let (pending, modules) = pending(src);
    let paths: Vec<String> = modules.iter().map(|m| m.path.clone()).collect();
    let items = modules.iter().map(|m| m.ast.items.len()).sum::<usize>();
    let pending = pending.expect("a kept check");
    // SAFETY: `modules` is the vector the check was given, alive and unchanged since.
    let kept = unsafe { pending.adopt(modules, vec![], None) };
    let kept = kept.ok().expect("the same modules");
    (kept, paths, items)
}

#[test]
fn a_kept_check_reads_its_modules_after_the_check() {
    let (kept, paths, items) = kept(
        "function twice(n: i64): i64 { return n * 2; }
         function main() { console.log(twice(2)); }",
    );
    // Every module the driver gave, then nothing it did not (no std loaded on demand here).
    assert_eq!(kept.module_paths(), paths);
    assert!(kept.items() >= items, "{} < {items}", kept.items());
    assert!(kept.definitions() > 0);
    assert!(!kept.owns_rewritten_modules());
    assert!(kept.borrows_only_what_it_owns());
}

#[test]
fn a_kept_check_owns_the_rewritten_modules_it_borrows() {
    // A closure nested in a closure and an overload make the source rewrites copy the modules:
    // the context then borrows the copy, which the kept check owns.
    let (kept, paths, _) = kept(
        "function pick(x: number): number;
         function pick(x: string): string;
         function pick(x: number | string): number | string { return x; }
         function main() {
           const outer = (n: number) => { const inner = (k: number) => k + n; return inner(1); };
           console.log(outer(2), pick(\"a\"));
         }",
    );
    assert!(kept.owns_rewritten_modules());
    assert!(kept.borrows_only_what_it_owns());
    let read = kept.module_paths().len();
    assert_eq!(read, paths.len());
    assert!(kept.items() > 0);
}

#[test]
fn a_kept_check_takes_over_only_the_modules_it_borrows() {
    let (pending, modules) = pending("function main() { console.log(1); }");
    let pending = pending.expect("a kept check");
    // The same program loaded again, at another address, is refused and comes back.
    let copy = load_src("function main() { console.log(1); }").modules;
    // SAFETY: refused before anything reads through it (another vector, at another address,
    // while the one checked is alive).
    let Err(back) = (unsafe { pending.adopt(copy, vec![], None) }) else {
        panic!("a copy of the modules was taken over");
    };
    assert_eq!(back.input.len(), modules.len());
}

#[test]
fn a_check_not_asked_to_keep_keeps_nothing() {
    let l = load_src("function main() { console.log(1); }");
    let (out, _) = velt_sema::check_generating(
        &l.modules,
        l.root,
        CheckOptions::default(),
        &GenInput::default(),
        &[],
        None,
        None,
    );
    assert!(matches!(out, Some(Checked::Program(_))));
    // The request is per check: a later check that asks gets its own.
    let (pending, _) = pending("function main() { console.log(2); }");
    assert!(pending.is_some());
}
