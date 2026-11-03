use super::*;

#[test]
fn missing_input_is_an_error_not_a_panic() {
    let mut sess = Session::new();
    let opts = BuildOptions {
        input: PathBuf::from("no/such/file.vlt"),
        ..Default::default()
    };
    match build(&mut sess, &opts) {
        Err(BuildError::Failed(m)) => assert!(m.contains("cannot read"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stops_after_first_failing_stage() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.vlt");
    std::fs::write(&file, "function main( {").unwrap();
    let mut sess = Session::new();
    let r = compile_to_vir(
        &mut sess,
        &BuildOptions {
            input: file,
            ..Default::default()
        },
    );
    assert!(matches!(r, Err(BuildError::Diagnostics)));
    assert!(sess.has_errors());
    assert_eq!(
        sess.timings.len(),
        1,
        "only the parse stage should have run"
    );
    assert!(sess.render_diagnostics().contains("bad.vlt:"));
}

#[test]
fn root_source_replaces_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        input: dir.path().join("virtual.vlt"),
        root_source: Some("function main( {".into()),
        ..Default::default()
    };
    let mut sess = Session::new();
    assert!(matches!(
        compile_to_vir(&mut sess, &opts),
        Err(BuildError::Diagnostics)
    ));
    assert!(sess.render_diagnostics().contains("virtual.vlt:"));
}

/// The compiler's memory is bounded by the stage it is in only when each stage's input is freed
/// once the stage is done: the HIR before the VIR is verified and optimized, the VIR before the
/// objects are written and linked.
#[test]
fn each_stage_frees_its_input_when_done() {
    let dir = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        input: dir.path().join("main.vlt"),
        root_source: Some("function main() {\n  console.log(\"hi\");\n}\n".into()),
        output: Some(dir.path().join("main.o")),
        emit: Emit::Obj,
        ..Default::default()
    };
    let mut sess = Session::new();
    let built = build(&mut sess, &opts);
    assert!(matches!(built, Ok(Artifact::Object(_))), "{built:?}");
    let events = sess.lifetimes.events();
    let at = |e: &str| {
        let i = events.iter().position(|x| *x == e);
        i.unwrap_or_else(|| panic!("no `{e}` in {events:?}"))
    };
    assert!(at("hir freed") < at("verify"), "{events:?}");
    assert!(at("vir freed") < at("write"), "{events:?}");
    assert!(at("optimize") < at("vir freed"), "{events:?}");
}
