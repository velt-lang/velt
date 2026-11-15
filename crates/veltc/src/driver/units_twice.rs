//! EXPERIMENT (#2943): `VELT_UNITS_TWICE` makes `velt check` check by units twice in this
//! process, std's unit kept between the checks (velt_sema `std_unit`), and report both times and
//! whether the checks differ: their diagnostics, or the lowering of their programs (a program's
//! maps print in any order). `VELT_UNITS_TWICE_DUMP=<dir>` writes both programs there.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use velt_sema::hir;

use super::{loader, lower_checked, BuildOptions, Session, VirFor};

/// Check `loaded` twice, with a key for its std modules' sources, which the build's own check
/// then takes too (`sema_opts.std_key`).
pub(super) fn run(
    sess: &mut Session,
    opts: &BuildOptions,
    loaded: &loader::Loaded,
    sema_opts: &mut velt_sema::CheckOptions,
    packages: Option<&vpm::PackageGraph>,
) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in loaded.modules.iter().filter(|m| m.is_std) {
        m.path.hash(&mut h);
        sess.sm.get(m.file).src.hash(&mut h);
    }
    sema_opts.std_key = Some(h.finish());
    let sema_opts = *sema_opts;
    let before = sess.diagnostics.len();
    let t1 = Instant::now();
    let first = crate::generate::check(
        sess,
        &loaded.modules,
        loaded.root,
        sema_opts,
        packages,
        None,
    );
    eprintln!("velt-units: first check {} ms", t1.elapsed().as_millis());
    let said: Vec<String> = sess
        .diagnostics
        .drain(before..)
        .map(|d| d.message)
        .collect();
    let t2 = Instant::now();
    let second = crate::generate::check(
        sess,
        &loaded.modules,
        loaded.root,
        sema_opts,
        packages,
        None,
    );
    eprintln!("velt-units: second check {} ms", t2.elapsed().as_millis());
    let again: Vec<String> = (sess.diagnostics[before..].iter())
        .map(|d| d.message.clone())
        .collect();
    if let Some(dir) = std::env::var_os("VELT_UNITS_TWICE_DUMP") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::write(dir.join("first.txt"), format!("{first:#?}"));
        let _ = std::fs::write(dir.join("second.txt"), format!("{second:#?}"));
    }
    // The programs compared by their lowering (a program's maps print in any order).
    let mut vir = |p: Option<hir::Program>| -> String {
        match p.map(|p| lower_checked(sess, opts, VirFor::Other, p)) {
            Some(Ok(v)) => v.to_string(),
            Some(Err(_)) => "lowering failed".into(),
            None => String::new(),
        }
    };
    let (v1, v2) = (vir(first), vir(second));
    if said != again || v1 != v2 {
        eprintln!("velt-units: the checks differ: {said:?} / {again:?}");
        if let Some(dir) = std::env::var_os("VELT_UNITS_TWICE_DUMP") {
            let dir = std::path::PathBuf::from(dir);
            let _ = std::fs::write(dir.join("first.vir"), &v1);
            let _ = std::fs::write(dir.join("second.vir"), &v2);
        }
    }
    sess.diagnostics.truncate(before);
}
