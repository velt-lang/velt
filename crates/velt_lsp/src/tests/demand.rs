//! The editor and `velt check` run one demand set (#2161, review point 6): over the real loader
//! and the real std, a document's check loads during the check what the command line's does, and
//! reports the same errors. Hooks are the demand kind std has so far; members and types come with
//! (a2) (#2161 step 4), and belong in `PROGRAMS` then.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use velt_common::{Diagnostic, Diagnostics, SourceMap};
use velt_loader::{load_more, load_program, LoadOptions, LoadState};
use velt_sema::generated::GenInput;
use velt_sema::{CheckOptions, Loads, SourceModule, StdLoads};

use crate::{LoadedProgram, ProgramLoader};

fn real_std() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

fn options<'a>(std: &Path, overlay: &'a HashMap<PathBuf, String>) -> LoadOptions<'a> {
    options_for(std, overlay, None)
}

/// [`options`] for the target triple `target` (`None`: this machine's), whose `#name` imports
/// pick std's variants.
fn options_for<'a>(
    std: &Path,
    overlay: &'a HashMap<PathBuf, String>,
    target: Option<&str>,
) -> LoadOptions<'a> {
    LoadOptions {
        std_root: Some(std.to_path_buf()),
        overlay: Some(overlay),
        demand_std: true,
        target: target.map(str::to_string),
        ..Default::default()
    }
}

/// The real loader, as `veltc`'s `CliLoader` drives it: a module that can't be served in the
/// middle of a check is refused, for the analysis to load it up front and check again.
struct RealLoader(PathBuf);

impl ProgramLoader for RealLoader {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        self.load_preloaded(root, overlay, &[], sm, diags)
    }

    fn load_preloaded(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        preload: &[String],
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        let opts = LoadOptions {
            preload,
            refuse_out_of_rule: true,
            ..options(&self.0, overlay)
        };
        let l = load_program(sm, root, opts, diags)?;
        Ok(LoadedProgram {
            modules: l.modules,
            root: l.root,
            more: Some(Box::new(l.state)),
        })
    }

    fn load_more(
        &self,
        _root: &Path,
        overlay: &HashMap<PathBuf, String>,
        state: &mut (dyn std::any::Any + Send),
        base: &[SourceModule],
        demanded: &[String],
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Option<velt_sema::StdBatch> {
        let state = state.downcast_mut::<LoadState>()?;
        let opts = LoadOptions {
            refuse_out_of_rule: true,
            ..options(&self.0, overlay)
        };
        Some(load_more(sm, state, base, demanded, &opts, diags))
    }
}

/// What the check of a program says, comparably: the errors, and the on-demand std modules it
/// holds at the end.
type Seen = (BTreeSet<String>, BTreeSet<String>);

fn errors(diags: &[Diagnostic], sm: &SourceMap) -> BTreeSet<String> {
    (diags.iter().filter(|d| d.is_error()))
        .map(|d| {
            let at = d.labels.first().map_or(0, |l| l.span.lo);
            let file = d.labels.first().map(|l| sm.get(l.span.file).path.clone());
            format!("{file:?}:{at}: {}", d.message)
        })
        .collect()
}

fn on_demand(paths: impl Iterator<Item = String>) -> BTreeSet<String> {
    paths
        .filter(|p| velt_common::std_hooks::ON_DEMAND_MODULES.contains(&p.as_str()))
        .collect()
}

/// `velt check`'s path: the driver's load, and its check with the load callback.
fn command_line(std: &Path, root: &Path, overlay: &HashMap<PathBuf, String>) -> Seen {
    command_line_for(std, root, overlay, None)
}

/// [`command_line`] for the target triple `target`.
fn command_line_for(
    std: &Path,
    root: &Path,
    overlay: &HashMap<PathBuf, String>,
    target: Option<&str>,
) -> Seen {
    command_line_linking(std, root, overlay, target, false)
}

/// Is hook `i` std's float printing (`number_shortest`), which a warm build cache links prebuilt?
fn prebuilt_shortest(i: usize) -> bool {
    velt_common::std_hooks::HOOKS[i].name == "number_shortest"
}

/// [`command_line_for`], with std's float printing linked prebuilt when `linked`, as a build
/// with a warm cache does.
fn command_line_linking(
    std: &Path,
    root: &Path,
    overlay: &HashMap<PathBuf, String>,
    target: Option<&str>,
    linked: bool,
) -> Seen {
    let mut sm = SourceMap::default();
    let mut diags = vec![];
    let mut l = load_program(&mut sm, root, options_for(std, overlay, target), &mut diags).unwrap();
    let modules = std::mem::take(&mut l.modules);
    let shared = Mutex::new((&mut sm, &mut l.state, &mut diags, vec![]));
    let serve = |d: &[String]| {
        let mut guard = shared.lock().ok()?;
        let (sm, state, diags, served) = &mut *guard;
        let b = load_more(
            sm,
            state,
            &modules,
            d,
            &options_for(std, overlay, target),
            diags,
        );
        served.extend(b.modules.iter().map(|m| m.path.clone()));
        Some(b)
    };
    let kept = StdLoads::default();
    let opts = CheckOptions {
        require_main: false,
        ..CheckOptions::default()
    };
    let (_, found) = velt_sema::check_generating(
        &modules,
        l.root,
        opts,
        &GenInput::default(),
        &[],
        None,
        Some(Loads {
            run: &serve,
            kept: &kept,
            prebuilt: linked.then_some(&prebuilt_shortest as &(dyn Fn(usize) -> bool + Sync)),
        }),
    );
    let (_, _, _, served) = shared.into_inner().unwrap();
    let held = modules.iter().map(|m| m.path.clone()).chain(served);
    let held = on_demand(held);
    diags.extend(found);
    (errors(&diags, &sm), held)
}

fn editor(std: &Path, root: &Path, overlay: &HashMap<PathBuf, String>) -> Seen {
    let mut folders = Default::default();
    let a = crate::analysis::analyze(&RealLoader(std.to_path_buf()), root, overlay, &mut folders);
    (
        errors(&a.diagnostics, &a.sm),
        on_demand(a.modules.iter().map(|m| m.path.clone())),
    )
}

/// `(program, the on-demand modules its check loads)`.
const PROGRAMS: &[(&str, &[&str])] = &[
    ("function main() {\n  console.log(\"hi\");\n}\n", &[]),
    (
        "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n",
        &["std/lang/hooks/number_text"],
    ),
    (
        "function main() {\n  const x: f64 = 0.5;\n  console.log(x);\n}\n",
        &["std/lang/hooks/number_write"],
    ),
    (
        "function show(x: number): string {\n  return `x=${x}`;\n}\nfunction main() {\n  console.log(show(1.5));\n}\n",
        &["std/lang/hooks/number_text", "std/lang/hooks/number_float"],
    ),
    (
        "function main() {\n  const a: any = { k: \"v\" };\n  const r = a as Record<string, string>;\n  console.log(r[\"k\"]);\n}\n",
        &["std/lang/hooks/any_record"],
    ),
    // A weak collection: std's weak core (#3145), which no other program loads.
    (
        "class K {\n  n: number = 1;\n}\nfunction main() {\n  const k = new K();\n  const r = new WeakRef(k);\n  console.log(r.deref() === k);\n}\n",
        &["std/lang/hooks/weak"],
    ),
    // An error with a demand: reported the same, wherever the module came from.
    (
        "function main() {\n  const n: i64 = \"text\";\n  console.log(`n=${n}`);\n}\n",
        &["std/lang/hooks/number_text"],
    ),
    // std calls the number printers and the long-decimal parser by role (`hookCall`, #3150).
    (
        "function main() {\n  console.log((1.5).toFixed(30), (1.5).toPrecision(25));\n}\n",
        &["std/lang/hooks/number_digits"],
    ),
    (
        "function main() {\n  console.log(parseInt(\"123456789012345678901234\"));\n}\n",
        &["std/lang/hooks/number_parse"],
    ),
    (
        "function main() {\n  const a: any = 2.5;\n  console.log(a.toFixed(30), Number(\"1.000000000000000000000001\"));\n}\n",
        &["std/lang/hooks/number_digits", "std/lang/hooks/number_parse"],
    ),
    // Methods written through interface values (#379): what they throw is inferred over the
    // whole program, and the writes some implementations can't store are refused, the same way.
    (
        "class F extends Error {}\ninterface C { update(): void; }\nconst c: C = { update: () => {} };\nc.update = () => { throw new F(\"x\"); };\ntry { c.update(); } catch (e) {}\n",
        &[],
    ),
    (
        "class F extends Error {}\nclass G extends Error {}\ninterface C { update(): void throws F; }\nconst c: C = { update: () => {} };\nc.update = () => { throw new G(\"x\"); };\n",
        &[],
    ),
    (
        "interface C { update(): void; }\nclass R implements C { update(): void {} }\nconst c: C = new R();\nc.update = () => {};\n",
        &[],
    ),
    // A std interface's method, written by user code: std's classes implement it with methods.
    (
        "const it: Iterator<number> = { next: () => ({ value: 1, done: true }) };\nit.next = () => ({ value: 2, done: true });\n",
        &[],
    ),
];

/// The number printers' and the long-decimal parser's modules: loaded where a program reaches
/// their hooks, also only through std, and reported by neither the editor nor `velt check` as
/// not loaded (#3150); a program that prints text, integers or floats loads neither.
#[test]
fn the_number_hook_modules_load_only_where_reached() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    let hooks = [
        "std/lang/hooks/number_digits",
        "std/lang/hooks/number_parse",
    ];
    for (k, (src, want)) in PROGRAMS.iter().enumerate() {
        let overlay = HashMap::from([(root.clone(), src.to_string())]);
        let std = real_std();
        for (errors, modules) in [
            command_line(&std, &root, &overlay),
            editor(&std, &root, &overlay),
        ] {
            assert!(
                !errors.iter().any(|e| e.contains("is not loaded")),
                "{src}: {errors:?}"
            );
            for h in hooks {
                // The first four reach no number hook (the conversion through `any` after them
                // does, by `Number(s)` and the like).
                match (want.contains(&h), k < 4) {
                    (true, _) => assert!(modules.contains(h), "{src}: {modules:?}"),
                    (false, true) => assert!(!modules.contains(h), "{src}: {modules:?}"),
                    (false, false) => {}
                }
            }
        }
    }
}

#[test]
fn the_editor_loads_and_reports_what_velt_check_does() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    for (src, _) in PROGRAMS {
        let overlay = HashMap::from([(root.clone(), src.to_string())]);
        let std = real_std();
        let (cli, ide) = (
            command_line(&std, &root, &overlay),
            editor(&std, &root, &overlay),
        );
        assert_eq!(cli.0, ide.0, "the errors of {src}");
        assert_eq!(cli.1, ide.1, "the modules of {src}");
    }
}

#[test]
fn each_program_demands_what_it_needs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    for (src, want) in PROGRAMS {
        let overlay = HashMap::from([(root.clone(), src.to_string())]);
        let want: BTreeSet<String> = want.iter().map(|s| s.to_string()).collect();
        let (_, modules) = editor(&real_std(), &root, &overlay);
        assert!(
            want.is_subset(&modules),
            "{src}: {modules:?}, wanted {want:?}"
        );
    }
}

/// `console.log`'s float writer has a module of its own: a program that prints no float doesn't
/// load it, and one that prints only a float loads neither the integer printing nor the float
/// text hooks with it (velt/lang/hooks/number_text), where std's float printing is linked
/// prebuilt; without that, std's float printing is checked, and demands the integer hooks.
#[test]
fn only_a_float_printed_loads_the_float_writer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    let (writer, integers) = ("std/lang/hooks/number_write", "std/lang/hooks/number_text");
    for (body, loads, loads_integers) in [
        ("  const x: f64 = 0.5;\n  console.log(x);", true, false),
        ("  console.log(0.5);", true, false),
        ("  const n: i64 = 7;\n  console.log(n);", false, true),
    ] {
        let src = format!("function main() {{\n{body}\n}}\n");
        let overlay = HashMap::from([(root.clone(), src)]);
        let (_, modules) = command_line_linking(&real_std(), &root, &overlay, None, true);
        assert_eq!(modules.contains(writer), loads, "{body}: {modules:?}");
        assert_eq!(
            modules.contains(integers),
            loads_integers,
            "{body}: {modules:?}"
        );
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (entry.path(), to.join(entry.file_name()));
        match p.is_dir() {
            true => copy_tree(&p, &q),
            false => {
                std::fs::copy(&p, &q).unwrap();
            }
        }
    }
}

/// The editor checks every body of the prelude, and still demands only what `velt check` would
/// reach: a conversion to a record in a prelude function nothing calls demands no record hooks.
/// The one module demanded is std's start-up check of the CPU (`std_hooks` `cpu_start`), which
/// lowering calls in every program's `velt_main` before `main`, so a program reaching nothing
/// still runs it.
#[test]
fn the_editor_demands_nothing_for_code_that_nothing_reaches() {
    let dir = tempfile::tempdir().unwrap();
    let std = dir.path().join("std");
    copy_tree(&real_std(), &std);
    let record = std.join("prelude/record.velt");
    let mut src = std::fs::read_to_string(&record).unwrap();
    src.push_str(
        "\nexport function unreachedConversion(x: any): Record<string, string> {\n  return x as Record<string, string>;\n}\n",
    );
    std::fs::write(&record, src).unwrap();
    let root = dir.path().join("main.velt");
    let overlay = HashMap::from([(root.clone(), "function main() {}\n".to_string())]);
    let (cli, ide) = (
        command_line(&std, &root, &overlay),
        editor(&std, &root, &overlay),
    );
    assert_eq!(cli, ide);
    let demanded: Vec<&str> = ide.1.iter().map(|m| m.as_str()).collect();
    assert_eq!(
        demanded,
        [velt_common::std_module!(lang_hooks_cpu)],
        "{:?}",
        ide.1
    );
}

/// Every program runs std's start-up check of the CPU, so what it reaches is in every program's
/// module set: it reaches no printing module on any target (on arm64 macOS a module constant of
/// a class once brought in the integer printer, `number_text`, for every program).
#[test]
fn the_start_up_check_loads_no_printing_module_on_any_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    let src = "function main() {}\n".to_string();
    let overlay = HashMap::from([(root.clone(), src)]);
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        let (errs, modules) = command_line_for(&real_std(), &root, &overlay, Some(target));
        assert!(errs.is_empty(), "{target}: {errs:?}");
        let demanded: Vec<&str> = modules.iter().map(|m| m.as_str()).collect();
        assert_eq!(
            demanded,
            [velt_common::std_module!(lang_hooks_cpu)],
            "{target}: the start-up check demands more than itself"
        );
    }
}

/// A document whose live code connects with TLS: the TLS client (std/net/tls/fd_client.velt)
/// can't be served in the middle of the check (velt_loader's (a1) rule), so the analysis loads it
/// up front and checks again, as `velt check` does, and reports nothing; one that can't connect
/// with TLS loads none of it.
#[test]
fn the_editor_checks_again_with_the_tls_client_up_front() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    let client = "std/net/tls/fd_client";
    for (src, loads) in [
        (
            "import { connectTls } from \"velt/net\";\nasync function main() {\n  const s = await connectTls(\"localhost:443\");\n  s.close();\n}\n",
            true,
        ),
        (
            "import { connect } from \"velt/net\";\nasync function main() {\n  const s = await connect(\"localhost:80\");\n  s.close();\n}\n",
            false,
        ),
    ] {
        let overlay = HashMap::from([(root.clone(), src.to_string())]);
        let mut folders = Default::default();
        let a = crate::analysis::analyze(&RealLoader(real_std()), &root, &overlay, &mut folders);
        let found = errors(&a.diagnostics, &a.sm);
        assert!(found.is_empty(), "{src}: {found:?}");
        assert_eq!(a.modules.iter().any(|m| m.path == client), loads, "{src}");
    }
}

/// A document that holds a vector through `velt/arch/x86`'s `Vector256` (which loads only part of
/// velt/simd), adds two of them, and connects with TLS (#3696): the `+` was looked up before the
/// TLS code brought the rest of velt/simd in the middle of the check, so the check refuses that
/// module after serving it (velt_sema `collect::vector_extends`, `Analysis::late_refused`) and the
/// analysis checks again with it up front, as `velt check` does. Without that, the abandoned
/// check's `+` error would be reported.
#[test]
fn the_editor_checks_again_after_a_late_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("main.velt");
    let src = r#"import { Vector256 } from "velt/arch/x86";
import { connectTls } from "velt/net";

async function main() {
  const bytes: u8[] = [];
  for (let i = 0; i < 32; i++) {
    bytes.push(i as u8);
  }
  const v = Vector256.loadU8(bytes).lower();
  const w = v + v;
  console.log(w.lane(3));
  const s = await connectTls("localhost:443");
  s.close();
}
"#;
    let overlay = HashMap::from([(root.clone(), src.to_string())]);
    let mut folders = Default::default();
    let a = crate::analysis::analyze(&RealLoader(real_std()), &root, &overlay, &mut folders);
    let found = errors(&a.diagnostics, &a.sm);
    assert!(found.is_empty(), "{found:?}");
    assert!(
        a.modules.iter().any(|m| m.path == "std/simd/ops_bytes"),
        "velt/simd's operators were not loaded"
    );
}
