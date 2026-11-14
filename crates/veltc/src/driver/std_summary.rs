//! `VELT_STD_SUMMARY=<file>`: the std identity oracle's output (#2943 phase 0). After a check, each
//! checked std definition is appended to `<file>` as `<name>\t<interface hash>\t<whole hash>`
//! (`velt_sema::interface::std_defs`), preceded by a `# <root file>` line. Comparing the lines of
//! `velt check --std` with those of a program shows the std definitions whose checked form depends
//! on the program (`bench/compile/std_identity.sh`). `VELT_STD_SUMMARY_SHOW=<part>` also writes the
//! hashed form of each std definition whose name contains `<part>` to `<file>.<name>.txt`.

use std::io::Write;

use velt_sema::hir;

/// Appends `hir`'s std definitions to the file `VELT_STD_SUMMARY` names, if it is set.
pub(super) fn write(hir: &hir::Program, root: &str) {
    let Some(path) = std::env::var_os("VELT_STD_SUMMARY") else {
        return;
    };
    if let Some(part) = std::env::var_os("VELT_STD_SUMMARY_SHOW") {
        let part = part.to_string_lossy();
        for (name, text) in velt_sema::interface::std_def_texts(hir, &part) {
            let file = format!(
                "{}.{}.txt",
                path.to_string_lossy(),
                name.replace(['/', ':'], "_")
            );
            if let Err(e) = std::fs::write(&file, text) {
                eprintln!("velt: VELT_STD_SUMMARY_SHOW: cannot write {file}: {e}");
            }
        }
    }
    let mut text = format!("# {root}\n");
    for d in velt_sema::interface::std_defs(hir) {
        text.push_str(&format!(
            "{}\t{:016x}\t{:016x}\n",
            d.name, d.interface, d.whole
        ));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    if let Err(e) = file.and_then(|mut f| f.write_all(text.as_bytes())) {
        eprintln!(
            "velt: VELT_STD_SUMMARY: cannot write {}: {e}",
            path.to_string_lossy()
        );
    }
}
