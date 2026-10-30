//! Source generators (docs/internals/design/source-generators.md): the driver's rounds.
//!
//! Sema checks the program; when it reaches instances of generated functions without code, it
//! returns their requests and the type model instead of a program ([`velt_sema::Checked`]). The
//! driver then builds the generators the requests name into one host program, runs it in a
//! child process (`velt __generate-host`, JIT-compiled as `velt dev --host` is), appends the
//! code it writes to the declaring modules, and checks again, until nothing is missing.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use serde_json::{json, Value};
use velt_common::{Diagnostic, Diagnostics, Span};
use velt_sema::generated::{spell, Demand, GenInput, GenInstance};
use velt_sema::{hir, CheckOptions, Checked, SourceModule};

use crate::driver::{self, BuildError, BuildOptions, Session};

/// The internal command a host runs as (`velt __generate-host <virtual root path>`).
pub const HOST_COMMAND: &str = "__generate-host";

/// The host program's source, for [`host_main`].
const ROOT_SOURCE_ENV: &str = "VELT_GENERATE_ROOT_SOURCE";

/// How many generator builds deep a build may be (a generator whose build needs generators):
/// past it, generators need each other in a cycle.
const DEPTH_ENV: &str = "VELT_GENERATE_DEPTH";
const MAX_DEPTH: u32 = 4;

/// At most this many rounds of checking and generating.
const MAX_ROUNDS: u32 = 8;

/// Where generated modules live in the source map (no file is written).
const GENERATED_DIR: &str = "<generated>";

/// Check `modules`, running generators until every instance of a generated function has code.
/// Returns the program, or `None` with the errors in `sess`.
pub(crate) fn check(
    sess: &mut Session,
    modules: &[SourceModule],
    root: usize,
    opts: CheckOptions,
) -> Option<hir::Program> {
    let mut input = GenInput::default();
    let mut owned: Option<Vec<SourceModule>> = None;
    // Per generated file: what it holds and the call that needed it, for errors in it.
    let mut files: Vec<(velt_common::FileId, Vec<Described>)> = vec![];
    for round in 1..=MAX_ROUNDS + 1 {
        let current = owned.as_deref().unwrap_or(modules);
        let t = Instant::now();
        let (out, diags) = velt_sema::check_generating(current, root, opts, &input);
        let diags = blame_generators(&sess.sm, diags, &files);
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
        let generated = run(sess, current, &demand, round);
        sess.timings.push(("generate", t.elapsed()));
        let generated = generated?;
        let mut next: Vec<SourceModule> = current.iter().map(copy_module).collect();
        for (module, text) in generated.modules {
            let Some(m) = next.iter_mut().find(|m| m.path == module) else {
                continue;
            };
            let path = PathBuf::from(GENERATED_DIR).join(format!("{module}.{round}.vlt"));
            emit(&path, &text);
            let file = sess.sm.add(path, text.clone());
            let described = (generated.described.iter())
                .filter(|d| d.module == module)
                .cloned()
                .collect();
            files.push((file, described));
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
            input.files.push(file);
        }
        input.instances.extend(generated.instances);
        owned = Some(next);
    }
    None
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
    }
}

/// What one round generated: source text per declaring module, and the instances in it.
struct Generated {
    modules: Vec<(String, String)>,
    instances: Vec<GenInstance>,
    described: Vec<Described>,
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
    files: &[(velt_common::FileId, Vec<Described>)],
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

/// One instance the host generated.
#[derive(Default)]
struct Written {
    key: String,
    function: String,
    args: Vec<u32>,
    body: Option<String>,
    helpers: String,
}

/// Run the generators for `demand`; `None` with the errors in `sess`.
fn run(
    sess: &mut Session,
    modules: &[SourceModule],
    demand: &Demand,
    round: u32,
) -> Option<Generated> {
    let depth: u32 = std::env::var(DEPTH_ENV)
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    let first_at = demand.requests.first().map_or(Span::DUMMY, |r| r.at);
    if depth >= MAX_DEPTH {
        let msg = "generators need generated code to be built, in a cycle";
        sess.diagnostics.push(Diagnostic::error(msg, first_at));
        return None;
    }
    // One key per generator module, by its file.
    let mut keys: Vec<PathBuf> = vec![];
    let mut fn_keys: Vec<String> = vec![];
    for f in &demand.functions {
        let Some(decl) = modules.iter().find(|m| m.path == f.module) else {
            fn_keys.push(String::new());
            continue;
        };
        let decl_path = sess.sm.get(decl.file).path.clone();
        let Some(path) = resolve_generator(&decl_path, &f.generator) else {
            let msg = format!(
                "the generator module `{}` must be a relative path (`./…`) for now",
                f.generator
            );
            sess.diagnostics
                .push(Diagnostic::error(msg, f.generator_span));
            return None;
        };
        let i = keys.iter().position(|k| *k == path).unwrap_or_else(|| {
            keys.push(path);
            keys.len() - 1
        });
        fn_keys.push(format!("g{i}"));
    }
    let dir = keys
        .first()
        .and_then(|k| k.parent())
        .map(Path::to_path_buf)?;
    // The host program: every generated function of every generator module the requests name.
    let mut src = String::from("import { serve } from \"velt/generate\";\n");
    for (i, k) in keys.iter().enumerate() {
        let spec = relative_spec(&dir, k);
        src.push_str(&format!("import * as g{i} from {spec:?};\n"));
    }
    src.push_str("\nfunction main() {\n  serve([\n");
    let mut entries = vec![];
    for (f, key) in demand.functions.iter().zip(&fn_keys) {
        if key.is_empty() || entries.contains(&(key.clone(), f.function.clone())) {
            continue;
        }
        src.push_str(&format!(
            "    {{ key: {key:?}, fn: {:?}, run: {key}.{} }},\n",
            f.function, f.function
        ));
        entries.push((key.clone(), f.function.clone()));
    }
    src.push_str("  ]);\n}\n");
    let prefix = format!("__gen_r{round}_");
    let request_names: Vec<String> = (demand.requests.iter().enumerate())
        .map(|(i, r)| format!("{prefix}{i}_{}", demand.functions[r.function].function))
        .collect();
    let input = json!({
        "prefix": prefix,
        "types": model_json(&demand.model),
        "functions": demand.functions.iter().zip(&fn_keys).map(|(f, key)| json!({
            "generator": key,
            "function": f.function,
            "params": f.params.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "requests": demand.requests.iter().zip(&request_names).map(|(r, name)| json!({
            "name": name,
            "generator": fn_keys[r.function],
            "function": demand.functions[r.function].function,
            "typeArgs": r.type_args,
        })).collect::<Vec<_>>(),
    });
    let host_root = dir.join("__velt_generate_host.vlt");
    let output = match spawn_host(&host_root, &src, &input.to_string(), depth + 1) {
        Ok(out) => out,
        Err(msg) => {
            let f = &demand.functions[demand.requests[0].function];
            let d = Diagnostic::error(format!("the generator `{}` failed", f.generator), first_at)
                .with_note(msg);
            sess.diagnostics.push(d);
            return None;
        }
    };
    let mut written: Vec<(String, Written)> = (demand.requests.iter().zip(&request_names))
        .map(|(r, name)| {
            let w = Written {
                key: fn_keys[r.function].clone(),
                function: demand.functions[r.function].function.clone(),
                args: r.type_args.clone(),
                ..Written::default()
            };
            (name.clone(), w)
        })
        .collect();
    let mut errors: Diagnostics = vec![];
    if let Err(msg) = parse_frames(&output, &mut written, &mut errors, demand, first_at) {
        let d =
            Diagnostic::error("a generator's output could not be read", first_at).with_note(msg);
        sess.diagnostics.push(d);
        return None;
    }
    if !errors.is_empty() {
        sess.diagnostics.extend(errors);
        return None;
    }
    // The code, per declaring module.
    let mut modules_out: Vec<(String, String)> = vec![];
    let mut instances = vec![];
    let mut described = vec![];
    for (name, w) in &written {
        let Some(fi) = (demand.functions.iter().zip(&fn_keys))
            .position(|(f, k)| *k == w.key && f.function == w.function)
        else {
            continue;
        };
        let f = &demand.functions[fi];
        let spellings: Vec<&str> = (w.args.iter())
            .map(|a| demand.model.types[*a as usize].spelling.as_str())
            .collect();
        let subst = |t: &str| {
            let mut t = t.to_string();
            for (i, s) in spellings.iter().enumerate() {
                t = t.replace(&spell::param_placeholder(i as u32), s);
            }
            t
        };
        let module_text = modules_out.iter().find(|(m, _)| *m == f.module);
        let start = module_text.map_or(0, |(_, t)| t.len()) as u32;
        let mut text = format!(
            "\n// {} for {}<{}>\n",
            f.generator,
            f.function,
            spellings.join(", ")
        );
        let mut type_args = vec![];
        for (i, s) in spellings.iter().enumerate() {
            let alias = format!("{name}_T{i}");
            text.push_str(&format!("type {alias} = {s};\n"));
            type_args.push(alias);
        }
        let params: Vec<String> = (f.params.iter())
            .map(|(p, t)| format!("{p}: {}", subst(t)))
            .collect();
        let asyncness = if f.is_async { "async " } else { "" };
        let throws = f
            .throws
            .as_ref()
            .map_or(String::new(), |t| format!(" throws {}", subst(t)));
        text.push_str(&format!(
            "{asyncness}function {name}({}): {}{throws} {{\n{}}}\n",
            params.join(", "),
            subst(&f.ret),
            w.body.as_deref().unwrap_or("")
        ));
        if !w.helpers.is_empty() {
            text.push_str(&w.helpers);
            text.push('\n');
        }
        match modules_out.iter_mut().find(|(m, _)| *m == f.module) {
            Some((_, t)) => t.push_str(&text),
            None => modules_out.push((f.module.clone(), text)),
        }
        let at = (demand.requests.iter().zip(&request_names))
            .find(|(_, n)| *n == name)
            .map_or(first_at, |(r, _)| r.at);
        let types: Vec<&str> = (w.args.iter())
            .map(|a| demand.model.types[*a as usize].display.as_str())
            .collect();
        let desc = format!(
            "`{}<{}>` by `{}`",
            f.function,
            types.join(", "),
            f.generator
        );
        let end = modules_out
            .iter()
            .find(|(m, _)| *m == f.module)
            .map_or(0, |(_, t)| t.len()) as u32;
        described.push(Described {
            module: f.module.clone(),
            range: (start, end),
            what: desc,
            at,
        });
        instances.push(GenInstance {
            module: f.module.clone(),
            function: f.function.clone(),
            type_args,
            body: name.clone(),
        });
    }
    Some(Generated {
        modules: modules_out,
        instances,
        described,
    })
}

/// The generator module file `spec` names, from the declaring module at `decl`.
fn resolve_generator(decl: &Path, spec: &str) -> Option<PathBuf> {
    if !spec.starts_with("./") && !spec.starts_with("../") {
        return None;
    }
    let mut p = decl.parent()?.join(spec);
    if p.extension().is_none() {
        p.set_extension("vlt");
    }
    Some(p)
}

/// An import specifier for `to` from a module in `dir`.
fn relative_spec(dir: &Path, to: &Path) -> String {
    let from: Vec<_> = dir.components().collect();
    let to = to.with_extension("");
    let target: Vec<_> = to.components().collect();
    let common = from.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec![];
    for _ in common..from.len() {
        parts.push("..".into());
    }
    for c in &target[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    let s = parts.join("/");
    if s.starts_with("..") {
        s
    } else {
        format!("./{s}")
    }
}

fn model_json(m: &velt_sema::generated::model::Model) -> Value {
    let opt = |o: Option<u32>| o.map_or(-1, i64::from);
    Value::Array(
        m.types
            .iter()
            .map(|t| {
                json!({
                    "kind": t.kind,
                    "display": t.display,
                    "qualifiedName": t.qualified_name,
                    "spelling": t.spelling,
                    "repr": t.repr,
                    "args": t.args,
                    "arms": t.arms,
                    "fields": t.fields.iter().map(|f| json!({
                        "name": f.name,
                        "type": f.ty,
                        "optional": f.optional,
                        "readonly": f.readonly,
                        "private": f.private,
                        "esPrivate": f.es_private,
                        "hasDefault": f.has_default,
                        "at": f.at,
                    })).collect::<Vec<_>>(),
                    "base": opt(t.base),
                    "subclasses": t.subclasses,
                    "members": t.members.iter().map(|m| json!({
                        "name": m.name,
                        "value": m.value,
                    })).collect::<Vec<_>>(),
                    "literal": t.literal,
                    "at": opt(t.at),
                })
            })
            .collect(),
    )
}

/// Run the host and return its standard output.
fn spawn_host(root: &Path, src: &str, input: &str, depth: u32) -> Result<Vec<u8>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find `velt`: {e}"))?;
    let mut child = Command::new(exe)
        .arg(HOST_COMMAND)
        .arg(root)
        .env(ROOT_SOURCE_ENV, src)
        .env(DEPTH_ENV, depth.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start the generator host: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let input = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut out = vec![];
    let mut err = vec![];
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let errs = std::thread::spawn(move || {
        let _ = stderr.read_to_end(&mut err);
        err
    });
    let _ = stdout.read_to_end(&mut out);
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = writer.join();
    let err = errs.join().unwrap_or_default();
    if !status.success() {
        let err = String::from_utf8_lossy(&err);
        return Err(format!(
            "the generator host exited with {status}:\n{}",
            err.trim_end()
        ));
    }
    Ok(out)
}

/// Read the host's records (`#<kind> <args…> <length>\n<text>\n`).
fn parse_frames(
    out: &[u8],
    written: &mut Vec<(String, Written)>,
    errors: &mut Diagnostics,
    demand: &Demand,
    first_at: Span,
) -> Result<(), String> {
    let mut pos = 0;
    let mut done = false;
    while pos < out.len() {
        let end = (out[pos..].iter().position(|&b| b == b'\n'))
            .map(|i| pos + i)
            .ok_or("a record without a header line")?;
        let header = std::str::from_utf8(&out[pos..end]).map_err(|e| e.to_string())?;
        let Some(header) = header.strip_prefix('#') else {
            // Something the generator printed: not ours.
            pos = end + 1;
            continue;
        };
        let mut words: Vec<&str> = header.split(' ').collect();
        let len: usize = words
            .pop()
            .and_then(|l| l.parse().ok())
            .ok_or("a record without a length")?;
        let text_start = end + 1;
        let text_end = text_start + len;
        let text = out
            .get(text_start..text_end)
            .ok_or("a record shorter than its length")?;
        let text = String::from_utf8_lossy(text).into_owned();
        pos = text_end + 1;
        let find = |written: &mut Vec<(String, Written)>, name: &str| {
            written.iter().position(|(n, _)| n == name)
        };
        match words.as_slice() {
            ["done"] => done = true,
            ["instance", name, key, function, args] => {
                let args = (args.split(',').filter(|a| !a.is_empty()))
                    .map(|a| a.parse::<u32>().map_err(|e| e.to_string()))
                    .collect::<Result<Vec<_>, _>>()?;
                let w = Written {
                    key: key.to_string(),
                    function: function.to_string(),
                    args,
                    ..Written::default()
                };
                written.push((name.to_string(), w));
            }
            ["instance", name, key, function] => {
                let w = Written {
                    key: key.to_string(),
                    function: function.to_string(),
                    ..Written::default()
                };
                written.push((name.to_string(), w));
            }
            ["body", name] => {
                let i = find(written, name).ok_or("a body of an unknown instance")?;
                written[i].1.body = Some(text);
            }
            ["helper", name] => {
                let i = find(written, name).ok_or("a helper of an unknown instance")?;
                written[i].1.helpers = text;
            }
            ["error", name, at] => {
                let at: i64 = at.parse().map_err(|_| "a bad location")?;
                // `-1`: at the call that needed the instance (its root request's, for an
                // instance a generator asked for).
                let call = (written.iter().position(|(n, _)| n == name))
                    .and_then(|i| demand.requests.get(i))
                    .map_or(first_at, |r| r.at);
                let span = usize::try_from(at)
                    .ok()
                    .and_then(|a| demand.model.spans.get(a).copied())
                    .unwrap_or(call);
                errors.push(Diagnostic::error(text, span));
            }
            _ => return Err(format!("an unknown record `#{header}`")),
        }
    }
    if !done {
        return Err("the generator host stopped before it finished".into());
    }
    Ok(())
}

/// `velt __generate-host <virtual root path>`: build the host program (its source in
/// [`ROOT_SOURCE_ENV`]) with the JIT and run it; exits with its exit code.
pub fn host_main(root: &Path) -> ExitCode {
    let Ok(src) = std::env::var(ROOT_SOURCE_ENV) else {
        crate::style::error("velt __generate-host is run by the compiler");
        return ExitCode::from(2);
    };
    let opts = BuildOptions {
        input: root.to_path_buf(),
        root_source: Some(src),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    let symbols = velt_rt_host::abi_symbols::symbol_table();
    let mut session = velt_codegen_cl::DevSession::new(&symbols);
    session.set_debug_info(false);
    let program = match driver::compile(&mut sess, &opts) {
        Ok(p) => p,
        Err(e) => {
            eprint!("{}", sess_report(&sess, &e));
            return ExitCode::from(1);
        }
    };
    let loaded = match session.load_timed(&program, &mut vec![]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("code generation failed: {e}");
            return ExitCode::from(1);
        }
    };
    velt_rt_host::process::set_args(vec![root.display().to_string()]);
    let code = velt_rt_host::entry::run_main(loaded.main());
    std::process::exit(code)
}

fn sess_report(sess: &Session, e: &BuildError) -> String {
    let mut out = String::new();
    for d in &sess.diagnostics {
        out.push_str(&d.render(&sess.sm));
        out.push('\n');
    }
    match e {
        BuildError::Diagnostics => {}
        BuildError::Failed(m) | BuildError::Ice(m) => out.push_str(&format!("error: {m}\n")),
    }
    out
}
