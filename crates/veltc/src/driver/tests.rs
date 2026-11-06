use super::*;

#[test]
fn missing_input_is_an_error_not_a_panic() {
    let mut sess = Session::new();
    let opts = BuildOptions {
        input: PathBuf::from("no/such/file.velt"),
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
    let file = dir.path().join("bad.velt");
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
    assert!(sess.render_diagnostics().contains("bad.velt:"));
}

#[test]
fn root_source_replaces_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        input: dir.path().join("virtual.velt"),
        root_source: Some("function main( {".into()),
        ..Default::default()
    };
    let mut sess = Session::new();
    assert!(matches!(
        compile_to_vir(&mut sess, &opts),
        Err(BuildError::Diagnostics)
    ));
    assert!(sess.render_diagnostics().contains("virtual.velt:"));
}

/// The compiler's memory is bounded by the stage it is in only when each stage's input is freed
/// once the stage is done: the HIR before the VIR is verified and optimized, the VIR before the
/// objects are written and linked.
#[test]
fn each_stage_frees_its_input_when_done() {
    let dir = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        input: dir.path().join("main.velt"),
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
    assert!(sess.memory.is_empty(), "read only for `--timings`");
}

/// The driver verifies the VIR after lowering and after optimizing, so the path it compiles
/// through (which frees the code) must not verify it again.
#[test]
fn codegen_does_not_verify_what_the_driver_verified() {
    let dir = tempfile::tempdir().unwrap();
    let opts = BuildOptions {
        input: dir.path().join("main.velt"),
        root_source: Some(
            "function main() {
  console.log(\"hi\");
}
"
            .into(),
        ),
        output: Some(dir.path().join("main.o")),
        emit: Emit::Obj,
        ..Default::default()
    };
    let mut sess = Session::new();
    sess.show_details = true;
    let built = build(&mut sess, &opts);
    assert!(matches!(built, Ok(Artifact::Object(_))), "{built:?}");
    let stages = sess.timings.iter().filter(|(n, _)| *n == "verify").count();
    assert_eq!(stages, 1, "{:?}", sess.timings);
    let steps: Vec<_> = sess.details.iter().filter(|d| d.1 == "verify").collect();
    assert!(steps.iter().all(|d| d.0 == "optimize"), "{steps:?}");
    let codegen = sess.memory.iter().any(|(s, _)| *s == "codegen");
    assert!(codegen, "{:?}", sess.memory);
}

/// Each line shows its own reading when a stage is recorded more than once, and a stage recorded
/// without one (`jit`) does not shift the rest.
#[test]
fn timings_show_each_recording_of_a_stage_its_own_memory() {
    let mb = |n: usize| memory::Reading {
        rss: n << 20,
        peak: n << 20,
    };
    let mut sess = Session::new();
    for stage in ["generate", "jit", "generate", "link"] {
        sess.timings.push((stage, Duration::ZERO));
    }
    sess.memory = vec![("generate", mb(1)), ("generate", mb(2)), ("link", mb(3))];
    let text = sess.render_timings();
    let ends: Vec<_> = text
        .lines()
        .map(|l| l.split("ms").nth(1).unwrap())
        .collect();
    let want = [
        "  (rss 1 MB, peak 1 MB)",
        "",
        "  (rss 2 MB, peak 2 MB)",
        "  (rss 3 MB, peak 3 MB)",
        "",
    ];
    assert_eq!(ends, want, "{text}");
}
