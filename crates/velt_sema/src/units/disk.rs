//! std's unit kept on disk between processes (`VELT_UNITS=2` with `VELT_UNITS_DISK=<dir>`,
//! #2943): a check whose driver gives a key of the compiler and std's sources
//! (`CheckOptions::std_key`) loads std's unit from `<dir>` instead of checking std, and saves the
//! unit its std's stage made there when there was none. The key is the driver's with what std's
//! stage takes of the program: the options it reads and the four flags of the source scans
//! (`Found`), and the encoding's format. A unit that does not decode (another format, damaged
//! bytes: its checksum) is a miss, and std is checked and saved again.

use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::units::codec;
use crate::{CheckOptions, SourceModule};

/// Where std's unit for this check is kept: `None` when the driver gave no key or
/// `VELT_UNITS_DISK` is not set. `found` is what the source scans found that std's stage reads.
pub(crate) fn path(opts: CheckOptions, found: [bool; 4]) -> Option<PathBuf> {
    let dir = std::env::var_os("VELT_UNITS_DISK")?;
    let std_key = opts.std_key?;
    let mut h = rustc_hash::FxHasher::default();
    std::hash::Hash::hash(
        &(std_key, opts.eager_std, opts.native_executor, found),
        &mut h,
    );
    std::hash::Hash::hash(codec::FORMAT.as_slice(), &mut h);
    let key = std::hash::Hasher::finish(&h);
    Some(PathBuf::from(dir).join(format!("{key:016x}.unit")))
}

/// std's unit from `path`, over std's `modules` (a load's, in any order), its AST in `arena`,
/// with where each of its modules is in `modules` (its own order is the one it was saved with);
/// `None` when there is none or it does not decode.
///
/// # Safety
///
/// `arena` must outlive the context returned and every copy of it (`codec::Arena`).
pub(crate) unsafe fn load<'m>(
    path: &Path,
    arena: &codec::Arena,
    modules: &[&'m SourceModule],
) -> Option<(Ctx<'m>, Vec<usize>)> {
    let t = std::time::Instant::now();
    let bytes = std::rc::Rc::new(std::fs::read(path).ok()?);
    let by_path: rustc_hash::FxHashMap<&str, usize> = (modules.iter().enumerate())
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let at: Option<Vec<usize>> = (codec::unit_module_paths(&bytes).unwrap_or_default().iter())
        .map(|p| by_path.get(p.as_str()).copied())
        .collect();
    let at = at.filter(|at| at.len() == modules.len());
    let ordered = (at.iter().flatten()).map(|&i| modules[i]).collect();
    // SAFETY: the caller keeps `arena` alive longer than the context.
    let cx = at
        .is_some()
        .then(|| unsafe { codec::decode_unit(&bytes, arena, ordered) })
        .flatten();
    eprintln!(
        "velt-units: std's unit {} from disk ({} bytes, {:.1} ms)",
        if cx.is_some() { "loaded" } else { "refused" },
        bytes.len(),
        t.elapsed().as_secs_f64() * 1e3,
    );
    Some((cx?, at?))
}

/// Keep std's unit `cx` at `path`, whole: written to a temporary file, then renamed into place,
/// so a check that reads it meanwhile reads the old unit or the new one.
pub(crate) fn save(path: &Path, cx: &Ctx) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = path
        .parent()
        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
        && std::fs::write(&tmp, codec::encode_unit(cx)).is_ok()
        && std::fs::rename(&tmp, path).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}
