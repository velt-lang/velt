//! A family that owns a reference besides its increment's keeps the release of that one
//! (#2455): removing every release would remove the owner's too, and nothing would free the
//! object. The increment and the release paired with it still go.

use super::*;

/// How the root got its pointer, and which releases follow the read.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Owner {
    /// `r = this.data`, released once: the field keeps its own reference (the control).
    Field,
    /// `r = this.data`, released through the copy and through `r`: `r` was the owner of what
    /// it read (an element moved out of its array).
    FieldReleasedTwice,
    /// `r` is a fresh block's object (`p = alloc(); *p = 1; r = p + 8`), released through the
    /// copy and through `r`: what a loop over a temporary array is once the temporary and the
    /// loop's copy of it are one register.
    Fresh,
    /// `Fresh` with a freeing call and a read through `r` between the two releases: the
    /// increment's reference is back by then, so the object's count is what it was.
    FreshUsedAfterTheCopy,
    /// `Fresh` with a freeing call before the read, while the increment's reference is held:
    /// the family's own reference keeps the object, and nothing else can give that one back.
    FreshFreeingCallWhileHeld,
    /// `FieldReleasedTwice` returning early between the releases: on that path the family had
    /// no reference of its own, so a freeing call while the increment's is held keeps the count.
    FieldReleasedTwiceOrNot,
    /// `FieldReleasedTwice` with a freeing call between the two releases: it runs once the
    /// increment's reference is back, so it is not looked at.
    FieldReleasedTwiceCallBetween,
    /// `r = this.data`, released through the copy on one path and through `r` on the other.
    FieldReleasedOnEitherPath,
}

/// `f(this) { r = …; r.count++; v = r; x = v.len; release v; [release r]; return x }`: which of
/// the increment and the releases, in that order, are gone.
fn owned(owner: Owner) -> Vec<bool> {
    let k = kit();
    let k_free = k.free;
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Bool], Ty::U64);
    let (this, either) = (fb.param(0), fb.param(1));
    let mut b = fb.block();
    let fresh = matches!(
        owner,
        Owner::Fresh | Owner::FreshUsedAfterTheCopy | Owner::FreshFreeingCallWhileHeld
    );
    let (r, q) = if fresh {
        let [p, r, q, n] = [Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::U64].map(|t| fb.local(t));
        let size = vec![int(32, Ty::U64), int(8, Ty::U64)];
        b = fb.call(b, Callee::Extern(k.alloc), size, Some(p));
        fb.push(b, Stmt::Assign(count(p), Rvalue::Use(int(1, Ty::U64))));
        fb.assign(b, r, bin(BinOp::PtrAdd, copy_local(p), int(8, Ty::I64)));
        fb.assign(b, q, bin(BinOp::PtrAdd, copy_local(r), int(-8, Ty::I64)));
        let add = bin(BinOp::Add, copy_place(count(q)), int(1, Ty::U64));
        fb.assign(b, n, add);
        fb.push(b, Stmt::Assign(count(q), Rvalue::Use(copy_local(n))));
        (r, q)
    } else {
        share(&mut fb, b, field_of(this, k.obj, 0))
    };
    let v = fb.local(Ty::Ptr);
    fb.assign(b, v, Rvalue::Use(copy_local(r)));
    if matches!(
        owner,
        Owner::FreshFreeingCallWhileHeld | Owner::FieldReleasedTwiceOrNot
    ) {
        b = fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None);
    }
    let x = read_len(&mut fb, &k, b, v);
    let mut qs = vec![q];
    if owner == Owner::FieldReleasedOnEitherPath {
        let [left, right, join] = [fb.block(), fb.block(), fb.block()];
        fb.branch(b, either, left, right);
        for (side, member) in [(left, v), (right, r)] {
            let (done, q2) = release(&mut fb, side, member, k.free);
            fb.goto(done, join);
            qs.push(q2);
        }
        b = join;
    } else {
        let (next, q2) = release(&mut fb, b, v, k.free);
        (b, qs) = (next, [qs, vec![q2]].concat());
        if owner == Owner::FreshUsedAfterTheCopy {
            b = fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None);
            read_len(&mut fb, &k, b, r);
        }
        if owner == Owner::FieldReleasedTwiceCallBetween {
            b = fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None);
        }
        if owner == Owner::FieldReleasedTwiceOrNot {
            let [early, on] = [fb.block(), fb.block()];
            fb.branch(b, either, early, on);
            fb.ret(early, copy_local(x));
            b = on;
        }
        if owner != Owner::Field {
            let (next, q3) = release(&mut fb, b, r, k.free);
            (b, qs) = (next, [qs, vec![q3]].concat());
        }
    }
    fb.ret(b, copy_local(x));
    let mut pb = k.pb;
    let f = pb.add(fb.finish());
    let mut p = pb.finish();
    let changed = run(&mut p)[f.0 as usize];
    assert_valid(&p);
    let func = &p.funcs[f.0 as usize];
    let stored = |q: &Local| {
        (func.blocks.iter().flat_map(|b| &b.stmts))
            .any(|s| matches!(s, Stmt::Assign(d, _) if count_word(d) == Some(*q)))
    };
    let gone: Vec<bool> = qs.iter().map(|q| !stored(q)).collect();
    assert_eq!(changed, gone.iter().any(|&g| g), "{owner:?}");
    // The object is freed where a release stays, and nowhere else.
    let frees = (func.blocks.iter())
        .filter(|b| matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. } if *e == k_free))
        .count();
    assert_eq!(
        frees,
        gone[1..].iter().filter(|&&g| !g).count(),
        "{owner:?}"
    );
    gone
}

#[test]
fn a_family_keeps_the_release_of_a_reference_of_its_own() {
    for (owner, gone) in [
        (Owner::Field, vec![true, true]),
        // Either release is the increment's on its path.
        (Owner::FieldReleasedOnEitherPath, vec![true, true, true]),
        // The copy's release is the increment's; the owner's stays and frees the object.
        (Owner::FieldReleasedTwice, vec![true, true, false]),
        (
            Owner::FieldReleasedTwiceCallBetween,
            vec![true, true, false],
        ),
        (Owner::Fresh, vec![true, true, false]),
        (Owner::FreshUsedAfterTheCopy, vec![true, true, false]),
        (Owner::FreshFreeingCallWhileHeld, vec![true, true, false]),
        (Owner::FieldReleasedTwiceOrNot, vec![false, false, false]),
    ] {
        assert_eq!(owned(owner), gone, "{owner:?}");
    }
}

/// `slot = &this.data; r = *slot; r.count++; c = r; [freeing(this)]; x = c.len; release c;
/// [c = *slot; *slot = null; release c]; return x`: whether the increment and its release
/// are gone. The bracketed release is of a field taken after the copy gave its reference
/// back, with no null test (a value that is never null).
fn field_taken(take: bool, freeing_while_held: bool) -> bool {
    let k = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
    let this = fb.param(0);
    let [slot, c] = [Ty::Ptr, Ty::Ptr].map(|t| fb.local(t));
    let mut b = fb.block();
    fb.assign(b, slot, Rvalue::AddrOf(field_of(this, k.obj, 0)));
    let field = deref(slot, Ty::Ptr);
    let (r, q) = share(&mut fb, b, field.clone());
    fb.assign(b, c, Rvalue::Use(copy_local(r)));
    if freeing_while_held {
        b = fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None);
    }
    let x = read_len(&mut fb, &k, b, c);
    let (mut b, q2) = release(&mut fb, b, c, k.free);
    if take {
        fb.assign(b, c, Rvalue::Use(copy_place(field.clone())));
        fb.push(b, Stmt::Assign(field, Rvalue::Use(int(0, Ty::Ptr))));
        let [q3, cnt, c1, m] = [Ty::Ptr, Ty::U64, Ty::Bool, Ty::U64].map(|t| fb.local(t));
        let [last, dec, exit] = [fb.block(), fb.block(), fb.block()];
        fb.assign(b, q3, bin(BinOp::PtrAdd, copy_local(c), int(-8, Ty::I64)));
        fb.assign(b, cnt, Rvalue::Use(copy_place(count(q3))));
        fb.assign(b, c1, bin(BinOp::Eq, copy_local(cnt), int(1, Ty::U64)));
        fb.branch(b, c1, last, dec);
        let args = vec![copy_local(q3), int(32, Ty::U64), int(8, Ty::U64)];
        let freed = fb.call(last, Callee::Extern(k.free), args, None);
        fb.goto(freed, exit);
        fb.assign(dec, m, bin(BinOp::Sub, copy_local(cnt), int(1, Ty::U64)));
        fb.push(dec, Stmt::Assign(count(q3), Rvalue::Use(copy_local(m))));
        fb.goto(dec, exit);
        b = exit;
    }
    fb.ret(b, copy_local(x));
    let mut pb = k.pb;
    let f = pb.add(fb.finish());
    decide(pb.finish(), f, &[[q, q2]])[0]
}

#[test]
fn a_field_taken_after_the_copy_does_not_make_the_family_owned() {
    // The field held the second reference while the copy was used, and `freeing` may replace
    // the field and release it: the copy keeps its count, taken afterwards or not.
    assert!(!field_taken(false, true));
    assert!(!field_taken(true, true));
    // With nothing that frees while the copy is used, it borrows either way.
    assert!(field_taken(false, false));
    assert!(field_taken(true, false));
}
