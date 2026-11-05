//! Building a package's C `sources` into a bundle (docs/internals/design/c-ffi.md "Where
//! libraries come from"): the pinned `zig cc` compiles each library for the target, from any
//! machine, into the static library release builds link and the shared library debug builds and
//! `velt dev` use. The same sources, on any machine and in any directory, give the same bytes:
//! fixed flags, paths relative to the package, no debug information, no timestamps. So a registry
//! or a CI can rebuild a bundle and compare its checksum.
//!
//! A bundle is reused while its key is the same ([`key`]): the file `<out>.key` beside it says what
//! it was built from, and zig runs again only when that changes.

use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};
use velt_toolchain::zig;

use crate::manifest::{CLibraryConfig, Manifest, C_SOURCES_DIR};
use crate::native::{
    c_library_files, c_library_name, CLibraryFiles, NativeLib, NativeMeta, NativeOrigin, META_FILE,
};

/// What every source is compiled with. A change here rebuilds every library ([`key`]).
///
/// One build serves debug and release programs alike, so it is optimized. `-fPIC` because it goes
/// into shared libraries and position-independent executables. No sanitizer, whatever zig's default
/// for the mode: a trap in a library that relies on what UBSan flags is no use, and a system
/// library's build has none. `-w`: warnings in a library are its author's, not the user's.
const CFLAGS: &[&str] = &[
    "-O2",
    "-g0",
    "-fPIC",
    "-ffunction-sections",
    "-fdata-sections",
    "-fno-sanitize=undefined",
    "-w",
    "-I.",
];

/// The most sources compiled at once, for one library, under the machine's core count.
const MAX_JOBS: usize = 16;

/// What to build.
#[derive(Clone, Debug)]
pub struct CBuildRequest<'a> {
    /// Package root (contains package.vlt and `native/`).
    pub root: &'a Path,
    /// Its manifest (with `sources` libraries).
    pub manifest: &'a Manifest,
    /// Target triple.
    pub target: &'a str,
    /// The bundle directory to write (replaced).
    pub out: &'a Path,
    /// How the result is recorded in the library's [`NativeLib`].
    pub origin: NativeOrigin,
}

/// Every `sources` library of `manifest` is in the bundle `meta` describes: a package's manifest
/// and its bundle come from one publish, but neither is taken on trust at link time.
pub fn check_has_libraries(
    manifest: &Manifest,
    meta: &NativeMeta,
    what: &str,
) -> Result<(), String> {
    for (lib, _) in manifest.c_source_libraries() {
        if !meta.c.contains_key(lib) {
            return Err(format!(
                "{what}: the bundle has no C library `{lib}`, which package.vlt builds from `sources`"
            ));
        }
    }
    Ok(())
}

/// The bundle of `req`: the one at `req.out` when its key is current, else built. One process at
/// a time checks and writes it, as for a crate's bundle.
pub fn build(req: CBuildRequest) -> Result<NativeLib, String> {
    let name = &req.manifest.package.name;
    if req.root.join(C_SOURCES_DIR).join("Cargo.toml").is_file() {
        return Err(format!(
            "package `{name}` has C `sources` and a Rust crate in `{C_SOURCES_DIR}/`: it has no way to build both yet"
        ));
    }
    let _lock = super::build::lock_bundle(req.out)?;
    let key = key(&req)?;
    let key_file = req.out.with_extension("key");
    let recorded = std::fs::read_to_string(&key_file).ok();
    if recorded.as_deref() == Some(key.as_str()) {
        if let Ok(lib) = NativeLib::open(req.out, req.origin.clone()) {
            return Ok(lib);
        }
    }
    let home = crate::locations::velt_home()?;
    let zig = zig::zig(&home)?;
    let cache = home.join("cache").join("zig").join("build-cache");
    let zig_target = zig::build_target(req.target).ok_or_else(|| {
        format!(
            "package `{name}`'s C sources can't be built for {}: zig has no such target",
            req.target
        )
    })?;
    let tmp = req.out.with_extension("tmp");
    clean(&tmp)?;
    let mut c = std::collections::BTreeMap::new();
    let built = (|| {
        for (lib, config) in req.manifest.c_source_libraries() {
            let tool = Tool {
                zig: &zig,
                cache: &cache,
                target: zig_target,
            };
            let files = build_library(&req, lib, config, &tool, &tmp)
                .map_err(|e| format!("package `{name}`, C library `{lib}`: {e}"))?;
            c.insert(lib.clone(), files);
        }
        let meta = NativeMeta {
            package: name.clone(),
            version: req.manifest.package.version.clone(),
            target: req.target.to_string(),
            // Calls no function of the runtime's table.
            abi: 0,
            shared: None,
            import_lib: None,
            static_obj: None,
            system_libs: None,
            exports: std::collections::BTreeMap::new(),
            c,
        };
        crate::json_file::write_atomic(&tmp.join(META_FILE), &meta.to_json())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    clean(req.out)?;
    std::fs::rename(&tmp, req.out)
        .map_err(|e| format!("cannot write `{}`: {e}", req.out.display()))?;
    // Best effort: without it the next build runs zig again.
    let _ = std::fs::write(&key_file, key);
    NativeLib::open(req.out, req.origin)
}

fn clean(dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("cannot clean `{}`: {e}", dir.display()))
        }
        _ => Ok(()),
    }
}

/// A hash of everything a bundle is built from: this build's flags and zig's version, the target,
/// the package's name and version, each library's name, sources and definitions, and every file
/// under `native/`, whichever of them the sources include.
fn key(req: &CBuildRequest) -> Result<String, String> {
    let mut h = Sha256::new();
    let mut add = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    add(zig::VERSION.as_bytes());
    add(CFLAGS.join(" ").as_bytes());
    add(req.target.as_bytes());
    add(req.manifest.package.name.as_bytes());
    add(req.manifest.package.version.as_bytes());
    for (lib, config) in req.manifest.c_source_libraries() {
        add(lib.as_bytes());
        for source in &config.sources {
            add(source.as_bytes());
        }
        for (name, value) in &config.defines {
            add(name.as_bytes());
            add(value.as_bytes());
        }
    }
    for file in native_files(req.root)? {
        add(file.as_bytes());
        let path = req.root.join(&file);
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
        add(&bytes);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The files under `native/` (relative to `root`, `/`-separated, sorted), as the package ships them.
pub fn native_files(root: &Path) -> Result<Vec<String>, String> {
    let mut files: Vec<String> = crate::contents::list_files(root)?
        .into_iter()
        .filter(|f| f.starts_with(&format!("{C_SOURCES_DIR}/")))
        .collect();
    files.sort();
    Ok(files)
}

struct Tool<'a> {
    zig: &'a Path,
    cache: &'a Path,
    target: &'a str,
}

impl Tool<'_> {
    /// `zig <args>` in `dir`, with zig's caches in one place and the clock fixed (C's `__DATE__`
    /// and `__TIME__`, a DLL's timestamp). `Err` holds what it printed.
    fn run(&self, dir: &Path, args: &[String]) -> Result<(), String> {
        let out = Command::new(self.zig)
            .args(args)
            .current_dir(dir)
            .env("ZIG_GLOBAL_CACHE_DIR", self.cache)
            .env("ZIG_LOCAL_CACHE_DIR", self.cache)
            .env("SOURCE_DATE_EPOCH", "0")
            .output()
            .map_err(|e| format!("cannot run `{}`: {e}", self.zig.display()))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "`zig {}` failed:\n{}",
            args.first().map(String::as_str).unwrap_or(""),
            stderr.trim()
        ))
    }
}

/// Compile `config`'s sources, and put the libraries of `lib` in the bundle directory `bundle`.
fn build_library(
    req: &CBuildRequest,
    lib: &str,
    config: &CLibraryConfig,
    tool: &Tool,
    bundle: &Path,
) -> Result<CLibraryFiles, String> {
    let objects = bundle.join("objects").join(lib);
    let mkdir = |d: &Path| {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create `{}`: {e}", d.display()))
    };
    mkdir(&objects)?;
    for source in &config.sources {
        if !req.root.join(source).is_file() {
            return Err(format!("source `{source}` is not in the package"));
        }
    }
    compile_all(req.root, config, tool, &objects)?;
    let names: Vec<String> = (0..config.sources.len()).map(object_name).collect();
    let files = c_library_files(&req.manifest.package.name, lib, req.target);
    mkdir(&bundle.join("shared"))?;
    if let Some(static_lib) = &files.static_lib {
        mkdir(&bundle.join("static"))?;
        // Named relative to the objects' directory, which the archive records.
        let mut args = vec!["ar".to_string(), "rcsD".to_string()];
        if req.target.contains("apple") {
            args.push("--format=darwin".to_string());
        }
        args.push(absolute(&bundle.join(static_lib))?);
        args.extend(names.iter().cloned());
        tool.run(&objects, &args)?;
    }
    link_shared(req, lib, &files, &names, tool, bundle, &objects)?;
    std::fs::remove_dir_all(bundle.join("objects"))
        .map_err(|e| format!("cannot clean `{}`: {e}", objects.display()))?;
    Ok(files)
}

/// `n.o`: a source's object, by its place in `sources` (two sources can share a file name).
fn object_name(n: usize) -> String {
    format!("{n}.o")
}

fn absolute(path: &Path) -> Result<String, String> {
    let path = crate::relpath::absolute(path);
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("`{}` is not valid UTF-8", path.display()))
}

/// Compile each of `config`'s sources, some at once, into `objects`. Each runs in the package's
/// root with its path relative to it, so no directory of this machine's reaches the object.
fn compile_all(
    root: &Path,
    config: &CLibraryConfig,
    tool: &Tool,
    objects: &Path,
) -> Result<(), String> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let jobs = cores.clamp(1, MAX_JOBS);
    let sources: Vec<(usize, &String)> = config.sources.iter().enumerate().collect();
    for batch in sources.chunks(jobs) {
        let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
            let running: Vec<_> = batch
                .iter()
                .map(|(n, source)| {
                    scope.spawn(move || {
                        let object = absolute(&objects.join(object_name(*n)))?;
                        tool.run(root, &compile_args(tool.target, config, source, &object))
                            .map_err(|e| format!("compiling `{source}`: {e}"))
                    })
                })
                .collect();
            running
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("a compile's thread panicked".to_string()))
                })
                .collect()
        });
        results.into_iter().collect::<Result<(), String>>()?;
    }
    Ok(())
}

fn compile_args(
    zig_target: &str,
    config: &CLibraryConfig,
    source: &str,
    object: &str,
) -> Vec<String> {
    let mut args: Vec<String> = ["cc", "-target", zig_target]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(CFLAGS.iter().map(|s| s.to_string()));
    args.extend(defines_args(&config.defines));
    args.extend(["-c".to_string(), source.to_string()]);
    args.extend(["-o".to_string(), object.to_string()]);
    args
}

/// `-DNAME=value` for each of `defines`.
pub fn defines_args(defines: &[(String, String)]) -> Vec<String> {
    defines
        .iter()
        .map(|(name, value)| format!("-D{name}={value}"))
        .collect()
}

/// Link the objects `names` of `lib` into its shared library. A library's name is its own on every
/// target: the executable records it, not this machine's path.
fn link_shared(
    req: &CBuildRequest,
    lib: &str,
    files: &CLibraryFiles,
    names: &[String],
    tool: &Tool,
    bundle: &Path,
    objects: &Path,
) -> Result<(), String> {
    let shared = bundle.join(&files.shared);
    let file = shared
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut args: Vec<String> = ["cc", "-target", tool.target, "-shared"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(["-o".to_string(), absolute(&shared)?]);
    args.extend(names.iter().cloned());
    if req.target.contains("apple") {
        args.push(format!("-Wl,-install_name,@rpath/{file}"));
    } else if let Some(import) = &files.import_lib {
        // Every function of the library, as elsewhere, without `__declspec(dllexport)`. clang
        // records zig's random temporary path in a Windows object whatever `-g0` says, so only
        // the DLL is reproducible: `-s` drops that record and the debug database, and
        // `SOURCE_DATE_EPOCH` sets its timestamp. Without `--out-implib`, the import library is
        // named after an object.
        args.extend(["-s".to_string(), "-Wl,--export-all-symbols".to_string()]);
        args.push(format!(
            "-Wl,--out-implib,{}",
            absolute(&bundle.join(import))?
        ));
    } else {
        args.extend([format!("-Wl,-soname,{file}"), "-lm".to_string()]);
    }
    tool.run(objects, &args)?;
    if let Some(import) = &files.import_lib {
        if !bundle.join(import).is_file() {
            return Err(format!(
                "zig wrote no import library for `{}`",
                c_library_name(&req.manifest.package.name, lib)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "import type { Package } from \"velt/package\";\nexport const pkg: Package = { name: \"p\", version: \"1.0.0\", native: { c: { tiny: { sources: [\"native/a.c\"], defines: { X: \"1\" } } } } };\n";

    /// What makes a bundle stale, and what doesn't; no zig runs for it.
    #[test]
    fn the_key_covers_what_a_library_is_built_from() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("native/target")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("package.vlt"), MANIFEST).unwrap();
        std::fs::write(root.join("native/a.c"), "int a;\n").unwrap();
        std::fs::write(root.join("native/a.inc"), "int b;\n").unwrap();
        let key_of = |text: &str, target: &str| {
            let manifest = Manifest::parse(text).unwrap();
            key(&CBuildRequest {
                root,
                manifest: &manifest,
                target,
                out: Path::new("unused"),
                origin: NativeOrigin::BuiltFromSource,
            })
            .unwrap()
        };
        let linux = "x86_64-unknown-linux-gnu";
        let base = key_of(MANIFEST, linux);
        assert_eq!(base, key_of(MANIFEST, linux));
        assert_ne!(base, key_of(MANIFEST, "aarch64-apple-darwin"));
        let changed = |text: &str| key_of(&MANIFEST.replace("X: \"1\"", text), linux);
        assert_ne!(base, changed("X: \"2\""));
        assert_ne!(base, changed("Y: \"1\""));
        assert_ne!(base, key_of(&MANIFEST.replace("1.0.0", "1.0.1"), linux));

        // Not built from these.
        std::fs::write(root.join("native/target/a.o"), "built").unwrap();
        std::fs::write(root.join("src/lib.vlt"), "changed").unwrap();
        assert_eq!(base, key_of(MANIFEST, linux));

        // Any file under `native/`, which a source can include, by its bytes.
        std::fs::write(root.join("native/a.inc"), "int c;\n").unwrap();
        let included = key_of(MANIFEST, linux);
        assert_ne!(base, included);
        std::fs::write(root.join("native/a.c"), "int z;\n").unwrap();
        let source = key_of(MANIFEST, linux);
        assert_ne!(included, source);
        std::fs::write(root.join("native/new.h"), "").unwrap();
        assert_ne!(source, key_of(MANIFEST, linux));
    }

    #[test]
    fn a_bundle_has_every_library_the_manifest_builds() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let mut meta = NativeMeta {
            package: "p".into(),
            version: "1.0.0".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            abi: 0,
            shared: None,
            import_lib: None,
            static_obj: None,
            system_libs: None,
            exports: Default::default(),
            c: Default::default(),
        };
        let e = check_has_libraries(&manifest, &meta, "`b`").unwrap_err();
        assert!(e.contains("no C library `tiny`"), "{e}");
        let files = c_library_files("p", "tiny", &meta.target);
        meta.c.insert("tiny".into(), files);
        check_has_libraries(&manifest, &meta, "`b`").unwrap();
    }

    #[test]
    fn a_compile_has_fixed_flags_and_a_relative_source() {
        let config = CLibraryConfig {
            defines: vec![("X".into(), "a b".into())],
            ..CLibraryConfig::default()
        };
        let args = compile_args("x86_64-linux-gnu", &config, "native/a.c", "/o/0.o");
        let all = args.join(" ");
        assert!(
            all.starts_with("cc -target x86_64-linux-gnu -O2 -g0"),
            "{all}"
        );
        assert!(all.ends_with("-c native/a.c -o /o/0.o"), "{all}");
        // One argument, whatever the value holds.
        assert!(args.contains(&"-DX=a b".to_string()), "{args:?}");
    }
}
