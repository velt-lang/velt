//! The member slots of the `any` tables in a program sema gave none (`hir::Program::dyn_members`
//! `None`): lowering adds them once a pass builds a call through one (`Boxing::any_members`).
//! Sema leaves them out where only std called by name uses a member of an `any`
//! (`FnCx::note_dyn_members`), so a `"k" in v` there, with no member use in the program's own
//! code, read past the table (#3492). Here the program's own `in` stands for std's: the same
//! program, with sema's answer taken away.

/// `src` checked with std, its `dyn_members` cleared as for a program whose only member use is
/// std's, then lowered, verified and interpreted.
fn run_without_sema_members(src: &str) -> super::interp::Outcome {
    let mut p = super::std_src::check_src(src);
    assert!(
        p.dyn_members.is_some(),
        "the program uses a member of an `any`"
    );
    p.dyn_members = None;
    super::run(&p)
}

#[test]
fn an_in_test_on_an_any_gets_the_has_slot() {
    let src = "\
const v: any = { k: 1 };
console.log(\"k\" in v, \"j\" in v);
";
    let out = run_without_sema_members(src);
    assert_eq!(out.stdout, "true false\n");
}
