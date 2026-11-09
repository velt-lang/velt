//! The intrinsics on raw memory (docs/internals/design/intrinsics.md "Memory at an address"):
//! `addressStore`, the `ptrAtomic*` operations, `atomicAddress`, `futPoll` and `futDrop`, as std
//! code calls them (only std may import the intrinsics).

mod common;

/// `src` checked as std code; the rendered diagnostics (empty when it checks).
fn std_diagnostics(src: &str) -> String {
    let mut l = common::programs::load_src(src);
    let root = l.root;
    l.modules[root].is_std = true;
    let (_, d) = l.check();
    l.render(&d)
}

const IMPORTS: &str = "import { Ptr } from \"velt/ffi\";
import { addressCall, addressLoad, addressStore, atomicAddress, fnAddress, futDrop, futPoll,
  ptrAtomicCas, ptrAtomicLoad, ptrAtomicRmw, ptrAtomicStore, promiseAt, slotPut, slotTake,
  stateDrop, statePoll }
  from \"velt/lang/intrinsics\";";

fn std_ok(body: &str) {
    let d = std_diagnostics(&format!("{IMPORTS}\n{body}"));
    assert!(!d.contains("error"), "{d}");
}

fn std_err(body: &str) -> String {
    let d = std_diagnostics(&format!("{IMPORTS}\n{body}"));
    assert!(d.contains("error"), "expected an error:\n{body}");
    d
}

#[test]
fn raw_memory_intrinsics_check_in_unsafe_code() {
    std_ok(
        "const W = new Atomic<u32>(0);
         class C { n: Atomic<u64> = new Atomic<u64>(0); inner: D = new D(); }
         class D { w: Atomic<i32> = new Atomic<i32>(0); }
         function main() {
           if (false) {
             unsafe {
               addressStore<u64>(8, 1);
               addressStore<bool>(8, true);
               addressStore<f32>(8, 1.5);
               const p = Ptr.at<u32>(atomicAddress(W));
               ptrAtomicStore(p, 1, \"release\");
               const v: u32 = ptrAtomicLoad(p, \"acquire\");
               const w: u32 = ptrAtomicRmw(p, \"add\", v, \"acqrel\");
               const x: u32 = ptrAtomicCas(p, w, 2, \"seqcst\", \"relaxed\");
               console.log(x, futPoll(16, 32));
               futDrop(16);
             }
             const s = shared(new C());
             console.log(atomicAddress(s.n), atomicAddress(s.inner.w));
           }
         }",
    );
}

#[test]
fn the_raw_writes_and_future_calls_need_unsafe_code() {
    let e = std_err("function main() { addressStore<u8>(8, 1); }");
    assert!(
        e.contains("calling unsafe intrinsic `addressStore` needs an `unsafe` block or function"),
        "{e}"
    );
    let e = std_err("function main() { console.log(futPoll(8, 16)); }");
    assert!(e.contains("calling unsafe intrinsic `futPoll`"), "{e}");
    let e = std_err("function main() { futDrop(8); }");
    assert!(e.contains("calling unsafe intrinsic `futDrop`"), "{e}");
    let e = std_err(
        "function main() { const p = unsafe { Ptr.at<u64>(8) }; console.log(ptrAtomicLoad(p, \"seqcst\")); }",
    );
    assert!(
        e.contains("calling unsafe intrinsic `ptrAtomicLoad`"),
        "{e}"
    );
}

#[test]
fn address_load_and_store_move_only_numbers_and_bools() {
    let e = std_err("function main() { unsafe { addressStore<string>(8, \"a\"); } }");
    assert!(
        e.contains("`addressStore<T>` needs a number type or `bool` for `T`, found `string`"),
        "{e}"
    );
    let e = std_err("function main() { unsafe { console.log(addressLoad<number[]>(8)); } }");
    assert!(
        e.contains("`addressLoad<T>` needs a number type or `bool`"),
        "{e}"
    );
    // A type parameter: an instance with a class would move an owned reference as bytes.
    let e = std_err(
        "function st<T>(a: usize, v: T) { unsafe { addressStore<T>(a, v); } }
         function ld<T>(a: usize): T { return unsafe { addressLoad<T>(a) }; }
         function main() {}",
    );
    assert!(
        e.contains("`addressStore<T>` needs a number type or `bool` for `T`, found `T`"),
        "{e}"
    );
    assert!(
        e.contains("`addressLoad<T>` needs a number type or `bool` for `T`, found `T`"),
        "{e}"
    );
}

#[test]
fn pointer_atomics_need_an_integer_and_literal_orderings() {
    let e = std_err(
        "function main() { unsafe { const p = Ptr.at<f64>(8); console.log(ptrAtomicLoad(p, \"seqcst\")); } }",
    );
    assert!(
        e.contains("`ptrAtomicLoad<T>` needs an integer type for `T`, found `f64`"),
        "{e}"
    );
    // An ordering the operation cannot have is the declaration's type error.
    let e = std_err(
        "function main() { unsafe { const p = Ptr.at<u32>(8); console.log(ptrAtomicLoad(p, \"release\")); } }",
    );
    assert!(e.contains("release"), "{e}");
    let e = std_err(
        "function order(): \"seqcst\" | \"relaxed\" { return \"relaxed\"; }
         function main() { unsafe { const p = Ptr.at<u32>(8); ptrAtomicStore(p, 1, order()); } }",
    );
    assert!(e.contains("an ordering must be a string literal"), "{e}");
    let e = std_err(
        "function op(): \"add\" | \"sub\" { return \"add\"; }
         function main() { unsafe { const p = Ptr.at<u32>(8); console.log(ptrAtomicRmw(p, op(), 1, \"seqcst\")); } }",
    );
    assert!(e.contains("an operation must be a string literal"), "{e}");
}

#[test]
fn atomic_address_takes_only_an_atomic_that_cannot_move() {
    let local =
        std_err("function main() { const a = new Atomic<u32>(0); console.log(atomicAddress(a)); }");
    assert!(
        local.contains("`atomicAddress` needs an `Atomic` that cannot move"),
        "{local}"
    );
    let object = std_err(
        "class C { n: Atomic<u64> = new Atomic<u64>(0); }
         function main() { const c = new C(); console.log(atomicAddress(c.n)); }",
    );
    assert!(object.contains("cannot move"), "{object}");
    let array = std_err(
        "class C { xs: Atomic<u64>[] = [new Atomic<u64>(0)]; }
         function main() { const s = shared(new C()); console.log(atomicAddress(s.xs[0])); }",
    );
    assert!(array.contains("cannot move"), "{array}");
    // A temporary box is freed at the end of the statement: its address would dangle.
    let call = std_err(
        "class C { n: Atomic<u64> = new Atomic<u64>(0); }
         function mk(): shared<C> { return shared(new C()); }
         function main() { console.log(atomicAddress(mk().n)); }",
    );
    assert!(call.contains("cannot move"), "{call}");
    let fresh = std_err(
        "class C { n: Atomic<u64> = new Atomic<u64>(0); }
         function main() { console.log(atomicAddress(shared(new C()).n)); }",
    );
    assert!(fresh.contains("cannot move"), "{fresh}");
    // A box in a variable or parameter outlives the call.
    std_ok(
        "class C { n: Atomic<u64> = new Atomic<u64>(0); }
         function f(s: shared<C>): usize { return atomicAddress(s.n); }
         function main() { const s = shared(new C()); console.log(atomicAddress(s.n) == f(s)); }",
    );
    // Module state is per task: a module `let` is not a static atomic.
    let state = std_err(
        "let A = new Atomic<u64>(0);
         function main() { console.log(atomicAddress(A)); }",
    );
    assert!(state.contains("cannot move"), "{state}");
    let narrow = std_err(
        "const B = new Atomic<u8>(0);
         function main() { console.log(atomicAddress(B)); }",
    );
    assert!(
        narrow.contains(
            "`atomicAddress` takes an `Atomic` of a 32- or 64-bit integer, found `Atomic<u8>`"
        ),
        "{narrow}"
    );
}

#[test]
fn fn_address_takes_a_function_c_can_call() {
    std_ok(
        "function poll(s: usize, cx: usize): u32 { return (s + cx) as u32; }
         function wake(w: usize): void {}
         function flag(b: bool, x: f64): bool { return b && x > 0; }
         function main() {
           const p = fnAddress(poll);
           // SAFETY: test code, never run.
           if (false) { unsafe { console.log(statePoll(p, 1, 2), addressCall(p, 3)); stateDrop(fnAddress(wake), 4); } }
           console.log(fnAddress(flag) !== 0);
         }",
    );
    for (decl, arg) in [
        ("function f(s: string): void {}", "f"),
        ("function f(): string { return \"\"; }", "f"),
        ("async function f(): Promise<void> {}", "f"),
        (
            "function f(x: u64): u64 throws Error { throw new Error(\"x\"); }",
            "f",
        ),
        ("function f(x: u64): u64 { return x; }", "(x: u64) => x"),
        ("class C { static m(x: u64): u64 { return x; } }", "C.m"),
    ] {
        let e = std_err(&format!(
            "{decl}\nfunction main() {{ console.log(fnAddress({arg})); }}"
        ));
        assert!(
            e.contains("module-level function C can call"),
            "{decl}: {e}"
        );
    }
}

#[test]
fn the_calls_through_an_address_need_unsafe_code() {
    for call in [
        "statePoll(8, 16, 24)",
        "stateDrop(8, 16)",
        "addressCall(8, 16)",
    ] {
        let e = std_err(&format!("function main() {{ console.log({call}); }}"));
        assert!(e.contains("unsafe"), "{call}: {e}");
    }
}

#[test]
fn slot_intrinsics_move_owned_values_in_unsafe_code() {
    std_ok(
        "function main() {
           if (false) {
             // SAFETY: test code, never run.
             unsafe {
               slotPut<string>(16, `moved ${1}`);
               const s: string = slotTake<string>(16);
               console.log(s);
               const p: Promise<void> = promiseAt<void>(32);
             }
           }
         }",
    );
}
