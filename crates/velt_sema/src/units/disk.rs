//! EXPERIMENT (`VELT_UNITS_DISK=<dir>`, #2943 M2): std's unit kept on disk between processes.
//! A check by units with the rewrites per unit (`VELT_UNITS_SPLIT`) whose std's sources have a
//! key (`CheckOptions::std_key`, which the driver gives) loads std's unit from `<dir>` instead of
//! checking std, and saves the unit its std's stage made there. The key is the driver's (the
//! compiler and std's sources) with what std's stage takes of the app: the four flags of the
//! source scans and the options it reads (`stubs::std_found`).

use std::path::PathBuf;

use crate::ctx::Ctx;
use crate::rewrite_facts::Found;
use crate::units::codec;
use crate::{CheckOptions, SourceModule};

/// Where std's unit for this check is kept, when the experiment is on.
pub(crate) fn path(opts: CheckOptions, std_found: Option<&Found>) -> Option<PathBuf> {
    let dir = std::env::var_os("VELT_UNITS_DISK")?;
    let (std_key, found) = (opts.std_key?, std_found?);
    let parts: [&dyn std::fmt::Debug; 3] = [
        &(opts.eager_std, opts.native_executor),
        &(
            found.null_proto,
            found.record_accessors,
            found.ctor_reads,
            found.gen_throw,
        ),
        &codec::FORMAT,
    ];
    let key = crate::std_unit::key(std_key, &parts);
    Some(PathBuf::from(dir).join(format!("{key:016x}.unit")))
}

/// std's unit from `path`, over std's `modules`, its AST in `arena`; `None` when there is none
/// or it does not decode (the check then checks std).
///
/// # Safety
///
/// `arena` must outlive the context returned and every copy of it (`codec::Arena`).
pub(crate) unsafe fn load<'m>(
    path: &std::path::Path,
    arena: &codec::Arena,
    modules: &'m [SourceModule],
) -> Option<Ctx<'m>> {
    let t = std::time::Instant::now();
    let bytes = std::fs::read(path).ok()?;
    let read = t.elapsed();
    // SAFETY: the caller keeps `arena` alive longer than the context.
    let cx = unsafe { codec::decode_unit(&bytes, arena, modules.iter().collect()) };
    eprintln!(
        "velt-units: std's unit {} from disk ({} bytes): read {:.1} ms, decoded {:.1} ms",
        if cx.is_some() { "loaded" } else { "refused" },
        bytes.len(),
        read.as_secs_f64() * 1e3,
        (t.elapsed() - read).as_secs_f64() * 1e3,
    );
    cx
}

/// Keep std's unit `cx` at `path` (written whole, then renamed into place).
pub(crate) fn save(path: &std::path::Path, cx: &Ctx) {
    let bytes = codec::encode_unit(cx);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = path
        .parent()
        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
        && std::fs::write(&tmp, &bytes).is_ok()
        && std::fs::rename(&tmp, path).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}
