//! Source generators (docs/internals/design/source-generators.md): the driver's rounds.
//!
//! Sema checks the program; when it reaches instances of generated functions without code, it
//! returns their requests and the type model instead of a program ([`velt_sema::Checked`]). The
//! driver then builds the generators the requests name into one host program ([`host`]), runs
//! it in a child process, appends the code it writes to the declaring modules, and checks again,
//! until nothing is missing.
//!
//! The first check is offered the modules the program's last build generated ([`cache`]): when
//! the program still needs exactly what they hold for the same types, that check is the only one.
//! An offer that does not cover the program (an instance missing or stale, an error in it) is
//! dropped, and the program is checked again from scratch, so a stale offer is never reported.

mod cache;
mod host;
mod run;

use std::path::{Path, PathBuf};
use std::time::Instant;

use velt_common::{Diagnostic, Diagnostics, FileId, Span};
use velt_sema::generated::{GenInput, GenInstance};
use velt_sema::{hir, CheckOptions, Checked, SourceModule};

use crate::driver::Session;

pub use host::host_main;

/// The internal command a JIT host runs as (`velt __generate-host <virtual root path>`).
pub const HOST_COMMAND: &str = "__generate-host";

/// At most this many rounds of checking and generating.
const MAX_ROUNDS: u32 = 8;

/// Where generated modules live in the source map (no file is written).
pub(crate) const GENERATED_DIR: &str = "<generated>";

/// Check `modules`, running generators until every instance of a generated function has code.
/// Returns the program, or `None` with the errors in `sess`.
pub(crate) fn check(
    sess: &mut Session,
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
) -> Option<hir::Program> {
    let declares = modules.iter().any(declares_generated);
    let root_path = modules
        .get(root)
        .map(|m| sess.sm.get(m.file).path.clone());
    let offer_key = (declares.then_some(root_path).flatten())
        .and_then(|p| cache::offer_key(&p, opts.require_main));
    if let Some(offer) = offer_key.as_ref().and_then(cache::load_offer) {
        let mark = sess.diagnostics.len();
        let files = offer.generator_files.clone();
        if let Some(p) = check_offered(sess, modules, root, opts, offer) {
            sess.generator_inputs.extend(files);
            return Some(p);
        }
        // Not covered: forget what checking with it reported, and start from scratch.
        sess.diagnostics.truncate(mark);
    }
    let mut st = Rounds::default();
    let program = rounds(sess, modules, root, opts, &mut st)?;
    if let Some(key) = offer_key.filter(|_| !st.offer.instances.is_empty()) {
        cache::store_offer(&key, &st.offer);
    }
    sess.generator_inputs
        .extend(std::mem::take(&mut st.offer.generator_files));
    Some(program)
}

/// One check of `modules` with the offered generated modules appended: the program when they
/// cover it without errors.
fn check_offered(
    sess: &mut Session,
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    offer: cache::Offer,
) -> Option<hir::Program> {
    let mut next: Vec<SourceModule> = modules.iter().map(copy_module).collect();
    let mut input = GenInput::default();
    for (module, name, text) in offer.files {
        let m = next.iter_mut().find(|m| m.path == module)?;
        let file = sess.sm.add(PathBuf::from(GENERATED_DIR).join(name), text.clone());
        let (ast, errors) = velt_syntax::parse_file(file, &text);
        if !errors.is_empty() {
            return None;
        }
        m.ast.items.extend(ast.items);
        input.files.push(file);
    }
    input.instances = offer.instances;
    let t = Instant::now();
    let (out, diags) = velt_sema::check_generating(&next, root, opts, &input);
    sess.timings.push(("sema", t.elapsed()));
    match out {
        Some(Checked::Program(p)) if !diags.iter().any(Diagnostic::is_error) => {
            sess.diagnostics.extend(diags);
            Some(*p)
        }
        _ => None,
    }
}

/// The state of [`rounds`]: what was generated so far.
#[derive(Default)]
struct Rounds {
    input: GenInput,
    /// Per generated file: what it holds and the call that needed it, for errors in it.
    files: Vec<(FileId, Vec<Described>)>,
    /// What to offer the next build.
    offer: cache::Offer,
}

fn rounds(
    sess: &mut Session,
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
    st: &mut Rounds,
) -> Option<hir::Program> {
    let mut owned: Option<Vec<SourceModule>> = None;
    for round in 1..=MAX_ROUNDS + 1 {
        let current = owned.as_deref().unwrap_or(modules);
        let t = Instant::now();
        let (out, diags) = velt_sema::check_generating(current, root, opts, &st.input);
        let diags = blame_generators(&sess.sm, diags, &st.files);
        sess.diagnostics.extend(diags);
        sess.timings.push((
            if round == 1 {
                "sema"
            } else {
                "sema (generated)"
            },
            t.elapsed(),
        ));
        let demand = match out {
            Some(Checked::Program(p)) => return Some(*p),
            Some(Checked::Demand(d)) => d,
            None => return None,
        };
        if round > MAX_ROUNDS {
            for r in &demand.requests {
                let f = &demand.functions[r.function];
                let msg = format!(
                    "`{}` still needs generated code after {MAX_ROUNDS} rounds of generating",
                    f.function
                );
                sess.diagnostics.push(Diagnostic::error(msg, r.at));
            }
            return None;
        }
        let t = Instant::now();
        let generated = run::run(sess, current, &demand, round);
        sess.timings.push(("generate", t.elapsed()));
        let generated = generated?;
        let mut next: Vec<SourceModule> = current.iter().map(copy_module).collect();
        for (module, text) in generated.modules {
            let Some(m) = next.iter_mut().find(|m| m.path == module) else {
                continue;
            };
            let name = format!("{module}.{round}.vlt");
            let path = PathBuf::from(GENERATED_DIR).join(&name);
            emit(&path, &text);
            let file = sess.sm.add(path, text.clone());
            let described = (generated.described.iter())
                .filter(|d| d.module == module)
                .cloned()
                .collect();
            st.files.push((file, described));
            let (ast, errors) = velt_syntax::parse_file(file, &text);
            if !errors.is_empty() {
                let at = demand.requests.first().map_or(Span::DUMMY, |r| r.at);
                let mut d = Diagnostic::error(
                    "the code a generator wrote does not parse (a bug in the generator)",
                    at,
                );
                for e in &errors {
                    d = d.with_note(e.message.clone());
                }
                sess.diagnostics.push(d);
                sess.diagnostics.extend(errors);
                return None;
            }
            m.ast.items.extend(ast.items);
            st.input.files.push(file);
            st.offer.files.push((module, name, text));
        }
        for (inst, fp) in generated.instances.iter().zip(generated.fingerprints) {
            st.offer.instances.push(GenInstance {
                fingerprint: fp,
                ..inst.clone()
            });
        }
        for f in generated.generator_files {
            if !st.offer.generator_files.contains(&f) {
                st.offer.generator_files.push(f);
            }
        }
        st.input.instances.extend(generated.instances);
        owned = Some(next);
    }
    None
}

/// Whether module `m` declares a generated function.
fn declares_generated(m: &SourceModule) -> bool {
    m.ast.items.iter().any(|i| {
        matches!(&i.kind, velt_syntax::ast::ItemKind::Function(f) if f.sig.generator.is_some())
    })
}

/// With [`EMIT_ENV`] set to a directory, generated modules are written there too, to read them
/// (the design's `--emit-generated`, until the CLI has it).
const EMIT_ENV: &str = "VELT_EMIT_GENERATED";

fn emit(path: &Path, text: &str) {
    let Some(dir) = std::env::var_os(EMIT_ENV) else {
        return;
    };
    let Ok(rel) = path.strip_prefix(GENERATED_DIR) else {
        return;
    };
    let out = PathBuf::from(dir).join(rel);
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(out, text);
}

fn copy_module(m: &SourceModule) -> SourceModule {
    SourceModule {
        path: m.path.clone(),
        is_std: m.is_std,
        file: m.file,
        ast: m.ast.clone(),
        imports: m.imports.clone(),
        jsx_runtime: m.jsx_runtime.clone(),
        is_dependency: m.is_dependency,
    }
}

/// What one round generated: source text per declaring module, and the instances in it.
struct Generated {
    modules: Vec<(String, String)>,
    instances: Vec<GenInstance>,
    /// Per instance: its fingerprint, for offers.
    fingerprints: Vec<String>,
    described: Vec<Described>,
    /// What the host's build read (build-stamp entries).
    generator_files: Vec<String>,
}

/// One instance in a generated file: where its code is, `fn<types>` by `<generator>`, and the
/// call that needed it.
#[derive(Clone)]
struct Described {
    module: String,
    range: (u32, u32),
    what: String,
    at: Span,
}

/// An error located in generated code is a bug in the generator: it is reported at the call
/// that needed the code, with the error in the generated file as a note.
fn blame_generators(
    sm: &velt_common::SourceMap,
    diags: Diagnostics,
    files: &[(FileId, Vec<Described>)],
) -> Diagnostics {
    diags
        .into_iter()
        .map(|d| {
            let Some(span) = d.labels.first().map(|l| l.span) else {
                return d;
            };
            let Some((_, described)) = files.iter().find(|(f, _)| *f == span.file) else {
                return d;
            };
            let Some(Described { what, at, .. }) = (described.iter())
                .find(|x| x.range.0 <= span.lo && span.lo < x.range.1)
                .or(described.first())
            else {
                return d;
            };
            if !d.is_error() {
                return d;
            }
            let (line, col) = sm.line_col(span.file, span.lo);
            let path = sm.get(span.file).path.display().to_string();
            let line_text = sm
                .get(span.file)
                .src
                .lines()
                .nth(line.saturating_sub(1))
                .unwrap_or("");
            let mut out = Diagnostic::error(
                format!("the code generated for {what} does not compile (a bug in the generator)"),
                *at,
            )
            .with_note(format!("{path}:{line}:{col}: {}", d.message))
            .with_note(format!("    {}", line_text.trim()));
            for n in d.notes {
                out = out.with_note(n);
            }
            out
        })
        .collect()
}

/// Generator hosts being built by this process, to report generators that need each other
/// instead of waiting for themselves. A JIT host (a child process) inherits the depth.
mod guard {
    use std::process::Command;
    use std::sync::Mutex;

    static BUILDING: Mutex<Vec<String>> = Mutex::new(vec![]);
    const DEPTH_ENV: &str = "VELT_GENERATE_DEPTH";
    const MAX_DEPTH: usize = 4;

    fn depth_from_env() -> usize {
        std::env::var(DEPTH_ENV)
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(0)
    }

    /// Whether host `src` is being built already (so building it again is a cycle).
    pub fn building(src: &str) -> bool {
        let b = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        b.iter().any(|s| s == src) || b.len() + depth_from_env() >= MAX_DEPTH
    }

    /// Run `f` while host `src` is being built.
    pub fn with<T>(src: &str, f: impl FnOnce() -> T) -> T {
        BUILDING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(src.to_string());
        let out = f();
        let mut b = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = b.iter().rposition(|s| s == src) {
            b.remove(i);
        }
        out
    }

    /// Give a JIT host process the current depth.
    pub fn pass_depth(cmd: &mut Command) {
        let b = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        cmd.env(DEPTH_ENV, (b.len() + depth_from_env()).to_string());
    }
}
