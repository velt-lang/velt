//! An executable's build (`velt build`, `velt run`): reused as it is while its stamp's inputs
//! are unchanged, compiled, linked and stamped otherwise.

use super::*;

pub(super) fn build_on_current_thread(
    sess: &mut Session,
    opts: &BuildOptions,
) -> Result<Artifact, BuildError> {
    let target = opts.target();
    // From before the stamp, which names objects a build at the same time could remove, through
    // the link. With no lock there yet, no build has placed any: its turn comes at placing them.
    let place =
        OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target).std_objects();
    if opts.emit == Emit::Exe && place.lock_exists() {
        sess.turn = place.lock(sess.verbose);
    }
    // An executable whose inputs are those of its last build is up to date (`stamp`).
    // The hashes of the files the last build's stamp records, for this build to reuse.
    let mut known = Default::default();
    // `--no-generators` checks the program again: an unchanged one may need generators.
    let stamps = !opts.report_numbers && !opts.no_generators && stamp::enabled();
    let stamped = (opts.emit == Emit::Exe && stamps).then(|| {
        let t = Instant::now();
        let backend = Backend::resolve(opts.backend, opts.release).0;
        let key = stamp::options_key(opts, backend);
        let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);
        let stamp_path = stamp::path(&paths.executable);
        let objects = stamp::read(&stamp_path, &key)
            .map(|(n, std, c_libs)| {
                // Not checked again, and the check is what finds them.
                sess.c_libs = c_libs;
                (0..n)
                    .map(|i| paths.unit_object(i))
                    .chain(std)
                    .collect::<Vec<_>>()
            })
            .filter(|objects| objects.iter().all(|o| o.is_file()));
        if objects.is_none() {
            known = stamp::known(&stamp_path);
            // A build that fails half-way must not leave a stamp of the old one.
            let _ = std::fs::remove_file(&stamp_path);
            // Made before the build looks at directories, so their listings include it.
            if let Some(dir) = paths.executable.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        sess.record("stamp", t);
        (key, paths, stamp_path, objects)
    });
    // A build that writes the executable without a stamp (`VELT_BUILD_STAMP=off`, `--report`)
    // leaves no stamp of an earlier one beside it.
    if stamped.is_none() && opts.emit == Emit::Exe {
        let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);
        let _ = std::fs::remove_file(stamp::path(&paths.executable));
    }
    if let Some((_, paths, _, Some(objects))) = &stamped {
        link_stage(sess, opts, &target, objects, paths)?;
        return Ok(Artifact::Executable(paths.executable.clone()));
    }
    // What module resolution looked at, for the stamp.
    let (program, seen) = match &stamped {
        Some(_) => vpm::probe::record_reusing(known, || {
            stamp::note_project(opts);
            compile_to_vir_linking(sess, opts, VirFor::Link)
        }),
        None => (
            compile_to_vir_linking(sess, opts, VirFor::Link),
            vpm::probe::Seen::default(),
        ),
    };
    let mut program = memory::Noted::new(program?, "vir freed", &sess.lifetimes);
    check_generator_host(sess, &program)?;
    // For the stamp: what the program reaches outside itself (`velt test` keeps a result only
    // for a program that reaches nothing).
    let reaches = match sess.reaches.take() {
        Some(reaches) => reaches,
        None => outside::reaches(&program),
    };
    if opts.emit == Emit::Vir {
        return Ok(Artifact::Vir(program.to_string()));
    }
    let cg = velt_codegen_cl::CodegenOptions {
        target: target.clone(),
        optimize: opts.release,
        cpu: opts.cpu.clone(),
    };
    let backend = Backend::of_build(opts);
    let required = backend
        .required_features(&target, opts.cpu.as_deref())
        .map_err(BuildError::Failed)?;
    cpu_check::insert(&mut program, required);
    if opts.emit == Emit::Llvm {
        return velt_codegen_llvm::emit_ir_for(&program, &cg)
            .map(Artifact::Llvm)
            .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")));
    }
    let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);

    let t = Instant::now();
    if backend == Backend::Cranelift && velt_codegen_llvm::is_wasm(&target) {
        return Err(BuildError::Failed(format!(
            "`{target}` needs the LLVM backend (`--backend llvm`)"
        )));
    }
    let mut steps = vec![];
    // `--emit obj` promises one object file.
    let units = if opts.emit == Emit::Obj {
        Some(1)
    } else {
        codegen_units()
    };
    let (objects, cached) = backend
        .emit_objects(&mut program, &cg, units, &mut steps)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")))?;
    // The objects are all the rest of the build needs: the VIR is freed before they are written
    // and linked.
    drop(program);
    sess.record("codegen", t);
    sess.cached = cached.map(|c| match backend {
        Backend::Cranelift => (c.hits, c.hits + c.misses, "functions"),
        Backend::Llvm => (c.hits, c.hits + c.misses, "codegen units"),
    });
    sess.record_details("codegen", &steps);

    let t = Instant::now();
    let mut object_paths = Vec::with_capacity(objects.len());
    // Each object is freed once written: the linker reads them from their files.
    for (i, obj) in objects.into_iter().enumerate() {
        let path = paths.unit_object(i);
        write_file(&path, &obj)?;
        object_paths.push(path);
    }
    sess.record("write", t);
    if opts.emit == Emit::Obj {
        // `-o lib.o` must not delete a `lib.cgu1.o` of the user's.
        return Ok(Artifact::Object(paths.object));
    }
    paths.remove_unit_objects_from(object_paths.len());

    // The std cache's objects are in place (`std_cache::link`); a build without them leaves
    // none of an earlier one's.
    if sess.std_objects.is_empty() {
        paths.std_objects().clear();
    }
    let mut linked = object_paths.clone();
    linked.extend(sess.std_objects.iter().cloned());
    link_stage(sess, opts, &target, &linked, &paths)?;
    crate::build_cache::collect_garbage_if_due();
    // Warnings are shown by every build, so a build with diagnostics gets no stamp.
    if let Some((key, _, stamp_path, _)) = stamped.filter(|_| sess.diagnostics.is_empty()) {
        let generated = opts.root_source.as_ref().map(|_| opts.input.as_path());
        // The outputs too: another build or tool that rewrites them (`--emit obj` onto a unit
        // object, an older velt) makes this stamp stale.
        let mut outputs = object_paths.clone();
        outputs.push(paths.executable.clone());
        let built = stamp::Built {
            seen: &seen,
            virtual_root: generated,
            objects: object_paths.len(),
            link_inputs: &outputs,
            std_objects: &sess.std_objects,
            outside: &reaches,
            generator_inputs: &sess.generator_inputs,
            c_libs: &sess.c_libs,
        };
        // Best effort: without a stamp the next build compiles again.
        let _ = stamp::write(&stamp_path, &stamp::render(&key, &sess.sm, &built));
    }
    Ok(Artifact::Executable(paths.executable))
}
