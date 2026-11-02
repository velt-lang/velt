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
