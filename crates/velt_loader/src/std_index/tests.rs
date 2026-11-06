use std::path::{Path, PathBuf};

use velt_build_cache::{Cache, Key};

use super::*;
use crate::std_ast::StdAstCache;

/// A std root in a temporary directory with `files` (path below the root, source).
fn std_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, src).unwrap();
    }
    tmp
}

fn real_std() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

fn module<'a>(index: &'a StdIndex, path: &str) -> &'a ModuleIndex {
    index
        .packages
        .iter()
        .flat_map(|p| &p.modules)
        .find(|m| m.path == path)
        .unwrap_or_else(|| panic!("no module {path}"))
}

fn package<'a>(index: &'a StdIndex, path: &str) -> &'a PackageIndex {
    index
        .packages
        .iter()
        .find(|p| p.path == path)
        .unwrap_or_else(|| panic!("no package {path}"))
}

const NET: &str = "\
import { Bytes } from \"velt/io/bytes\";
import \"./tcp\";
export { Socket } from \"./socket\";
export * from \"#transport\";
export function connect(host: string): void {}
export const PORT = 80, [A, ...REST] = [1, 2];
export struct Addr { host: string }
export class Conn {}
export interface Readable { read(): i64; }
export enum Mode { A, B }
export type Port = u16;
export declare function velt_rt_tcp(fd: i64): i64;
function hidden(): void {}
extend<T> Array<T> {
  first(): T { return this[0]; }
  static of1(x: T): T[] { return [x]; }
}
extend Addr { text(): string { return this.host; } }
";

#[test]
fn packages_modules_exports_imports_and_extends() {
    let tmp = std_with(&[
        ("net/index.velt", NET),
        ("net/tcp.velt", "export class Tcp {}\n"),
        ("net/impl/socket.velt", "export class Socket {}\n"),
        ("net/http/index.velt", "export const X = 1;\n"),
        ("imports.velt", "export const imports = 1;\n"),
    ]);
    let index = load(tmp.path(), None);
    let paths: Vec<(&str, Vec<&str>)> = index
        .packages
        .iter()
        .map(|p| {
            (
                p.path.as_str(),
                p.modules.iter().map(|m| m.path.as_str()).collect(),
            )
        })
        .collect();
    // An implementation folder (`impl/`) belongs to its package; a folder with an `index.velt`
    // is a package of its own; files in no package are `std`'s.
    assert_eq!(
        paths,
        [
            ("std", vec!["std/imports"]),
            (
                "std/net",
                vec!["std/net", "std/net/impl/socket", "std/net/tcp"]
            ),
            ("std/net/http", vec!["std/net/http"]),
        ]
    );
    let net = module(&index, "std/net");
    assert_eq!(
        net.imports,
        [
            "std/io/bytes",
            "std/net/tcp",
            "std/net/socket",
            "#transport"
        ]
    );
    let exports: Vec<(&str, ExportKind, Option<&str>)> = net
        .exports
        .iter()
        .map(|e| (e.name.as_str(), e.kind, e.from.as_deref()))
        .collect();
    assert_eq!(
        exports,
        [
            ("Socket", ExportKind::Name, Some("std/net/socket")),
            ("*", ExportKind::All, Some("#transport")),
            ("connect", ExportKind::Function, None),
            ("PORT", ExportKind::Var, None),
            ("A", ExportKind::Var, None),
            ("REST", ExportKind::Var, None),
            ("Addr", ExportKind::Struct, None),
            ("Conn", ExportKind::Class, None),
            ("Readable", ExportKind::Interface, None),
            ("Mode", ExportKind::Enum, None),
            ("Port", ExportKind::TypeAlias, None),
            ("velt_rt_tcp", ExportKind::Extern, None),
        ]
    );
    let extends: Vec<(&str, &str, bool)> = net
        .extends
        .iter()
        .map(|e| (e.ty.as_str(), e.member.as_str(), e.is_static))
        .collect();
    assert_eq!(
        extends,
        [
            ("Array", "first", false),
            ("Array", "of1", true),
            ("Addr", "text", false)
        ]
    );
    // `extend Array` is another module's type; `extend Addr` is its own.
    assert_eq!(
        net.violations,
        [Violation::ExtendsForeignType { ty: "Array".into() }]
    );
    assert!(module(&index, "std/net/tcp").in_place());
}

#[test]
fn hook_roles_come_from_the_hook_table() {
    let index = load(&real_std(), None);
    let promise = module(&index, "std/host/promise");
    assert_eq!(
        promise.hooks,
        [
            "host_settled",
            "host_await",
            "host_await_void",
            "host_await_ref",
            "host_await_ref_or_null"
        ]
    );
    assert!(promise.imports.contains(&"std/io/error".to_string()));
    assert!(promise.in_place(), "{:?}", promise.violations);
    let text = module(&index, "std/lang/hooks/number_text");
    for hook in ["int_text", "uint_text", "int_push", "uint_push"] {
        assert!(
            text.hooks.iter().any(|h| h == hook),
            "{hook}: {:?}",
            text.hooks
        );
    }
    assert!(text.extends.is_empty());
    // Every hook of the table is in the index, at its module.
    let indexed: usize = index
        .packages
        .iter()
        .flat_map(|p| &p.modules)
        .map(|m| m.hooks.len())
        .sum();
    assert_eq!(indexed, velt_common::std_hooks::HOOKS.len());
    // The prelude's `extend string` adds `string`'s methods, and is not served in place.
    let string = module(&index, "std/prelude/string");
    assert!(string
        .extends
        .iter()
        .any(|e| e.ty == "string" && e.member == "padStart" && !e.is_static));
    assert!(string.violations.contains(&Violation::ExtendsForeignType {
        ty: "string".into()
    }));
    assert!(string.exports.iter().any(|e| e.name == "parseInt"));
}

#[test]
fn the_encoding_round_trips_and_rejects_other_formats() {
    let index = load(&real_std(), None);
    assert!(index.packages.len() > 40, "{}", index.packages.len());
    for p in &index.packages {
        let bytes = codec::encode(p);
        assert_eq!(codec::decode(&bytes).as_ref(), Some(p), "{}", p.path);
        // Another format: a miss.
        let mut other = bytes.clone();
        other[7] ^= 1;
        assert_eq!(codec::decode(&other), None);
        // Cut short, or with bytes after it: a miss.
        assert_eq!(codec::decode(&bytes[..bytes.len() - 1]), None);
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(codec::decode(&longer), None);
    }
    // Damaged anywhere: never a panic.
    let bytes = codec::encode(package(&index, "std/prelude"));
    for i in (FORMAT.len()..bytes.len()).step_by(7) {
        let mut damaged = bytes.clone();
        damaged[i] = 0xff;
        let _ = codec::decode(&damaged);
    }
}

fn cache_of(dir: &Path) -> Cache {
    Cache::new(dir, u64::MAX)
}

/// The files of std package `rel` (one `index.velt`) under `root`, as the index keys them.
fn files_of(root: &Path, rel: &str) -> Vec<store::StdFile> {
    let path = root.join(rel).join("index.velt");
    let src = std::fs::read_to_string(&path).unwrap();
    vec![store::StdFile {
        canonical: crate::locate::Origin::Std(root.to_path_buf()).canonical(&path),
        path,
        src,
    }]
}

fn with(cache: &Cache, compiler: Key) -> Option<StdAstCache<'_>> {
    Some(StdAstCache { cache, compiler })
}

#[test]
fn a_package_is_built_on_a_miss_and_read_on_a_hit() {
    let tmp = std_with(&[
        ("a/index.velt", "export function f(): void {}\n"),
        ("b/index.velt", "export function g(): void {}\n"),
    ]);
    let c1 = Key::of(&[b"c1"]);
    let prefix = store::prefix(&c1);
    let key_a = store::key(&prefix, "std/a", &files_of(tmp.path(), "a"));
    let fresh = load(tmp.path(), None);
    // A miss builds the index and stores it, one entry per package, under its sources' key.
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_of(dir.path());
    assert_eq!(load(tmp.path(), with(&cache, c1)), fresh);
    let stored = cache.get_key(KIND, &key_a).and_then(|b| codec::decode(&b));
    assert_eq!(stored.as_ref(), Some(package(&fresh, "std/a")));
    // A hit is what the entry holds: plant another index under `std/a`'s key.
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_of(dir.path());
    let mut planted = package(&fresh, "std/a").clone();
    planted.modules[0].hooks.push("planted".into());
    cache.put_key(KIND, &key_a, &codec::encode(&planted));
    let warm = load(tmp.path(), with(&cache_of(dir.path()), c1));
    assert_eq!(package(&warm, "std/a"), &planted);
    assert_eq!(package(&warm, "std/b"), package(&fresh, "std/b"));
    // Another compiler, or an edited file: another key, built again.
    let c2 = Key::of(&[b"c2"]);
    assert_eq!(
        package(&load(tmp.path(), with(&cache, c2)), "std/a"),
        package(&fresh, "std/a")
    );
    std::fs::write(
        tmp.path().join("a/index.velt"),
        "export function h(): void {}\n",
    )
    .unwrap();
    let edited = load(tmp.path(), with(&cache, c1));
    assert_eq!(module(&edited, "std/a").exports[0].name, "h");
}

#[test]
fn an_entry_that_does_not_decode_is_a_miss() {
    let tmp = std_with(&[("a/index.velt", "export function f(): void {}\n")]);
    let c1 = Key::of(&[b"c1"]);
    let prefix = store::prefix(&c1);
    let a = files_of(tmp.path(), "a");
    let key_a = store::key(&prefix, "std/a", &a);
    let fresh = load(tmp.path(), None);
    let other_package = PackageIndex {
        path: "std/b".into(),
        modules: vec![],
    };
    let mut other_format = codec::encode(package(&fresh, "std/a"));
    other_format[7] ^= 1;
    let entries = [
        b"not an index".to_vec(),
        other_format,
        // An index of another package (no key can name two): a miss too.
        codec::encode(&other_package),
    ];
    for entry in entries {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_of(dir.path());
        cache.put_key(KIND, &key_a, &entry);
        assert_eq!(load(tmp.path(), with(&cache, c1)), fresh);
    }
}

#[test]
fn a_package_that_does_not_parse_is_not_stored() {
    let tmp = std_with(&[("a/index.velt", "export function f( {\n")]);
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_of(dir.path());
    let c1 = Key::of(&[b"c1"]);
    let index = load(
        tmp.path(),
        Some(StdAstCache {
            cache: &cache,
            compiler: c1,
        }),
    );
    assert_eq!(index.packages.len(), 1);
    assert_eq!(cache.usage().entries, 0, "nothing stored");
}

/// Loads a std of `files` through one cache, applies `change` to it and loads again with that
/// cache: the second load must be what a load with no cache gives, not the first load's entry.
/// Returns the two loads.
fn through_one_cache(files: &[(&str, &str)], change: impl FnOnce(&Path)) -> (StdIndex, StdIndex) {
    let tmp = std_with(files);
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_of(dir.path());
    let c1 = Key::of(&[b"c1"]);
    let before = load(tmp.path(), with(&cache, c1));
    // Each file's parse, and the package's entry.
    assert_eq!(cache.usage().entries, files.len() as u64 + 1, "stored");
    change(tmp.path());
    let after = load(tmp.path(), with(&cache, c1));
    assert_eq!(after, load(tmp.path(), None));
    (before, after)
}

fn module_paths(index: &StdIndex) -> Vec<&str> {
    index
        .packages
        .iter()
        .flat_map(|p| &p.modules)
        .map(|m| m.path.as_str())
        .collect()
}

fn export_names(m: &ModuleIndex) -> Vec<&str> {
    m.exports.iter().map(|e| e.name.as_str()).collect()
}

#[test]
fn a_renamed_file_with_the_same_contents_is_another_entry() {
    let (before, after) = through_one_cache(
        &[
            ("a/index.velt", "export const A = 1;\n"),
            ("a/x.velt", "export const X = 1;\n"),
        ],
        |root| std::fs::rename(root.join("a/x.velt"), root.join("a/y.velt")).unwrap(),
    );
    assert_eq!(module_paths(&before), ["std/a", "std/a/x"]);
    assert_eq!(module_paths(&after), ["std/a", "std/a/y"]);
}

/// An element in a `.velt` file, whose text looks like an item; in a `.ts` file, the assertion
/// `<a>b` and then that item (`</ a>`: a `</a` would make it an element there too). The canonical
/// path is the same: only the plain-TypeScript flag tells the two files apart.
const JSX_OR_ASSERTION: &str = "export const el = <a>b\nexport const k = 1;\n</ a>;\n";

#[test]
fn a_file_that_changes_only_its_extension_to_ts_is_another_entry() {
    let (before, after) = through_one_cache(
        &[
            ("a/index.velt", "export const A = 1;\n"),
            ("a/x.velt", JSX_OR_ASSERTION),
        ],
        |root| std::fs::rename(root.join("a/x.velt"), root.join("a/x.ts")).unwrap(),
    );
    assert_eq!(module_paths(&before), module_paths(&after));
    assert_eq!(export_names(module(&before, "std/a/x")), ["el"]);
    assert!(
        export_names(module(&after, "std/a/x")).contains(&"k"),
        "{:?}",
        module(&after, "std/a/x").exports
    );
}

#[test]
fn a_file_added_to_a_package_is_another_entry() {
    let (before, after) = through_one_cache(&[("a/index.velt", "export const A = 1;\n")], |root| {
        std::fs::write(root.join("a/z.velt"), "export const Z = 1;\n").unwrap()
    });
    assert_eq!(module_paths(&before), ["std/a"]);
    assert_eq!(module_paths(&after), ["std/a", "std/a/z"]);
    assert_eq!(export_names(module(&after, "std/a/z")), ["Z"]);
}
