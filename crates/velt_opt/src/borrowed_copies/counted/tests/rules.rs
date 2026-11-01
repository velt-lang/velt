//! Tests that each rule of the analysis is needed: release glue only, uses through pointers
//! into the object, and the runtime functions taken as freeing nothing.

use super::*;

#[test]
fn a_member_address_handed_to_a_function_that_is_not_glue_keeps_the_count() {
    // The same with `release_db(&v)`, a Velt function with glue's body that is not drop glue:
    // only glue is known to be the release, so the address taken counts as escaping.
    let mut k = kit();
    let other = release_fn(&mut k, "release_db");
    assert_eq!(released_through(k, other, false), (false, true));
}

#[test]
fn a_pointer_into_the_object_read_after_a_freeing_call_keeps_the_count() {
    // `r = this.data; r.count++; ip = &r.len; [freeing(this)]; y = *ip; release r; return y`:
    // the read through `ip` is a use of the object, so a call that may free before it keeps
    // the count.
    for (free, write, borrows) in [
        (false, false, true),
        (true, false, false),
        (false, true, true),
        (true, true, false),
    ] {
        let mut k = kit();
        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
        let this = fb.param(0);
        let [ip, y] = [Ty::Ptr, Ty::U64].map(|t| fb.local(t));
        let mut b = fb.block();
        let (r, q) = share(&mut fb, b, field_of(this, k.obj, 0));
        fb.assign(b, ip, Rvalue::AddrOf(field_of(r, k.arr, 1)));
        if free {
            b = fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None);
        }
        if write {
            fb.push(
                b,
                Stmt::Assign(deref(ip, Ty::U64), Rvalue::Use(int(5, Ty::U64))),
            );
            fb.assign(b, y, Rvalue::Use(int(0, Ty::U64)));
        } else {
            fb.assign(b, y, Rvalue::Use(copy_place(deref(ip, Ty::U64))));
        }
        let (exit, q2) = release(&mut fb, b, r, k.free);
        fb.ret(exit, copy_local(y));
        let f = k.pb.add(fb.finish());
        let p = k.pb.finish();
        assert_eq!(
            decide(p, f, &[[q, q2]]),
            [borrows],
            "free: {free}, write: {write}"
        );
    }
}

/// Every runtime function taken as freeing no counted object is a real `velt_rt` export: a
/// rename cannot leave a stale name in the safe set while the new name is taken as freeing,
/// nor move a freeing function into it unnoticed.
#[test]
fn every_function_taken_as_freeing_nothing_is_a_runtime_export() {
    fn sources(dir: &std::path::Path, out: &mut String) {
        for e in std::fs::read_dir(dir).expect("velt_rt sources").flatten() {
            let path = e.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    let mut rt = String::new();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../velt_rt/src");
    sources(&dir, &mut rt);
    for name in NO_FREE.iter().chain(super::super::super::READ_ONLY.iter()) {
        assert!(
            rt.contains(&format!("fn {name}(")),
            "`{name}` is not defined in velt_rt"
        );
    }
}
