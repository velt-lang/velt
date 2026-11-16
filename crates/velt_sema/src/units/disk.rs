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
pub(crate) fn path(opts: CheckOptions, found: Option<&Found>) -> Option<PathBuf> {
    let dir = std::env::var_os("VELT_UNITS_DISK")?;
    let (std_key, found) = (opts.std_key?, found?);
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

/// std's unit from `path`, over std's `modules` (in the order the unit had them, which the
/// second result gives as positions in `modules`: a load's order depends on the program), its AST
/// in `arena`; `None` when there is none or it does not decode (the check then checks std).
///
/// # Safety
///
/// `arena` must outlive the context returned and every copy of it (`codec::Arena`).
pub(crate) unsafe fn load<'m>(
    path: &std::path::Path,
    arena: &codec::Arena,
    modules: &[&'m SourceModule],
) -> Option<(Ctx<'m>, Vec<usize>)> {
    let t = std::time::Instant::now();
    let bytes = std::rc::Rc::new(std::fs::read(path).ok()?);
    let read = t.elapsed();
    let by_path: rustc_hash::FxHashMap<&str, usize> = (modules.iter().enumerate())
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();
    let order: Option<Vec<usize>> = (codec::unit_module_paths(&bytes)?.iter())
        .map(|p| by_path.get(p.as_str()).copied())
        .collect();
    let order = order.filter(|o| o.len() == modules.len())?;
    let ordered = order.iter().map(|&i| modules[i]).collect();
    // SAFETY: the caller keeps `arena` alive longer than the context.
    let cx = unsafe { codec::decode_unit(&bytes, arena, ordered) };
    eprintln!(
        "velt-units: std's unit {} from disk ({} bytes): read {:.1} ms, decoded {:.1} ms",
        if cx.is_some() { "loaded" } else { "refused" },
        bytes.len(),
        read.as_secs_f64() * 1e3,
        (t.elapsed() - read).as_secs_f64() * 1e3,
    );
    Some((cx?, order))
}

/// What a check with std's modules as stubs (`CheckOptions::std_stubbed`) reports when std's
/// unit is not on disk for it: the driver loads std and checks again.
pub const UNIT_NEEDED: &str = "velt-units: std's unit is not on disk for this check";

/// Keep std's unit `cx` at `path`, and the stubs of std's modules (from `modules`, the modules
/// the app's rewrites ran over) beside it under std's sources' key (`std_key`), for a load that
/// reads no std AST ([`read_stubs`]).
pub(crate) fn save(
    path: &std::path::Path,
    cx: &Ctx,
    modules: Option<&[SourceModule]>,
    std_key: u64,
) {
    write_whole(path, &codec::encode_unit(cx));
    let stubs = path.with_file_name(format!("{std_key:016x}.stubs"));
    // Written again by every check that saves a unit: the modules in it may have changed
    // (the dependencies an app reaches, `units::libraries`).
    if let Some(modules) = modules {
        let mut out = STUBS_FORMAT.to_vec();
        for m in modules.iter().filter(|m| crate::units::in_first_unit(m)) {
            let ast = velt_syntax::encode_module(&m.ast, m.file);
            for part in [m.path.as_bytes(), &ast[..]] {
                out.extend_from_slice(&(part.len() as u64).to_le_bytes());
                out.extend_from_slice(part);
            }
        }
        write_whole(&stubs, &out);
    }
}

/// Write `bytes` to `path` whole: to a temporary file, then renamed into place.
fn write_whole(path: &std::path::Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = path
        .parent()
        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
        && std::fs::write(&tmp, bytes).is_ok()
        && std::fs::rename(&tmp, path).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}

const STUBS_FORMAT: &[u8; 8] = b"vstub\x00\x00\x01";

/// The stubs of std's modules kept under std's sources' key `std_key` in `dir` ([`save`]): each
/// module's path with its stub, encoded (`velt_syntax::decode_module` decodes it for the file
/// the module has in a load); `None` when there are none.
pub fn read_stubs(dir: &std::path::Path, std_key: u64) -> Option<Vec<(String, Vec<u8>)>> {
    let bytes = std::fs::read(dir.join(format!("{std_key:016x}.stubs"))).ok()?;
    let mut rest = bytes.strip_prefix(STUBS_FORMAT.as_slice())?;
    let mut take = || -> Option<Vec<u8>> {
        let (n, tail) = rest.split_first_chunk::<8>()?;
        let n = usize::try_from(u64::from_le_bytes(*n)).ok()?;
        let (part, tail) = (tail.get(..n)?, tail.get(n..)?);
        rest = tail;
        Some(part.to_vec())
    };
    let mut out = vec![];
    while let Some(path) = take() {
        out.push((String::from_utf8(path).ok()?, take()?));
    }
    Some(out)
}
