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
        &(
            std_key,
            opts.eager_std,
            opts.native_executor,
            opts.jit,
            found,
        ),
        &mut h,
    );
    std::hash::Hash::hash(codec::FORMAT.as_slice(), &mut h);
    let key = std::hash::Hasher::finish(&h);
    Some(PathBuf::from(dir).join(format!("{key:016x}.unit")))
}

/// Can the program change what std's stage makes of std's modules? Then its check neither loads
/// std's unit from disk nor keeps the one it makes: the key ([`path`]) is of std's sources, not
/// of what a program adds to them. A program does when it
/// - augments a module (`declare module "…" { interface … }`: `interface_merge` adds the
///   members, and imports of the program's types, to the module's own interface, std's too);
/// - has overloads, or a class named as one of std's (`overloads::reaches_std`);
/// - defines a symbol brand or a global namespace (`brands`, `global_namespaces`: facts of the
///   whole program that std's check reads, a hidden key in a spread, a path no module resolves).
///
/// The other source rewrites make a module's code from that module alone, or leave std's
/// modules as they are. `modules` are the load's, before the rewrites.
pub(crate) fn shaped_by_program(
    modules: &[SourceModule],
    brands: &crate::defined_props::Brands,
    global_namespaces: &crate::namespaces::GlobalNamespaces,
) -> bool {
    use velt_syntax::ast::ItemKind;
    let augments = |m: &SourceModule| {
        (m.ast.items.iter()).any(|i| matches!(&i.kind, ItemKind::Augment(a) if a.module.is_some()))
    };
    !brands.sites.is_empty()
        || !brands.direct.is_empty()
        || !global_namespaces.is_empty()
        || modules.iter().any(|m| !m.is_std && augments(m))
        || crate::overloads::reaches_std(modules)
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
