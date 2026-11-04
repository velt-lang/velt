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
    LoadOptions {
        std_root: Some(std.to_path_buf()),
        overlay: Some(overlay),
        demand_std: true,
        ..Default::default()
    }
}

/// The real loader, as `veltc`'s `CliLoader` drives it.
struct RealLoader(PathBuf);

impl ProgramLoader for RealLoader {
    fn load(
        &self,
        root: &Path,
        overlay: &HashMap<PathBuf, String>,
        sm: &mut SourceMap,
        diags: &mut Diagnostics,
    ) -> Result<LoadedProgram, String> {
        let l = load_program(sm, root, options(&self.0, overlay), diags)?;
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
        Some(load_more(
            sm,
            state,
            base,
            demanded,
            &options(&self.0, overlay),
            diags,
        ))
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
    let mut sm = SourceMap::default();
    let mut diags = vec![];
    let mut l = load_program(&mut sm, root, options(std, overlay), &mut diags).unwrap();
    let modules = std::mem::take(&mut l.modules);
    let shared = Mutex::new((&mut sm, &mut l.state, &mut diags, vec![]));
    let serve = |d: &[String]| {
        let mut guard = shared.lock().ok()?;
        let (sm, state, diags, served) = &mut *guard;
        let b = load_more(sm, state, &modules, d, &options(std, overlay), diags);
        served.extend(b.modules.iter().map(|m| m.path.clone()));
        Some(b)
    };
    let kept = StdLoads::default();
    let opts = CheckOptions {
        require_main: false,
        eager_std: false,
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
        "function show(x: number): string {\n  return `x=${x}`;\n}\nfunction main() {\n  console.log(show(1.5));\n}\n",
        &["std/lang/hooks/number_text", "std/lang/hooks/number_float"],
    ),
    (
        "function main() {\n  const a: any = { k: \"v\" };\n  const r = a as Record<string, string>;\n  console.log(r[\"k\"]);\n}\n",
        &["std/lang/hooks/any_record"],
    ),
    // An error with a demand: reported the same, wherever the module came from.
    (
        "function main() {\n  const n: i64 = \"text\";\n  console.log(`n=${n}`);\n}\n",
        &["std/lang/hooks/number_text"],
    ),
];

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
    assert!(ide.1.is_empty(), "{:?}", ide.1);
}
