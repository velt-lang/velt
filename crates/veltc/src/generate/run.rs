//! One round of generating: the host program for the generators a demand names, its input
//! (the requests and the type model), its output read back into generated modules.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use velt_common::{Diagnostic, Diagnostics, Span};
use velt_sema::generated::{spell, Demand, GenInstance};
use velt_sema::SourceModule;

use super::{cache, host, Described, Generated, GeneratedText};
use crate::driver::Session;

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
pub(super) fn run(
    sess: &mut Session,
    modules: &[SourceModule],
    demand: &Demand,
    round: u32,
    packages: Option<&vpm::PackageGraph>,
) -> Option<Generated> {
    let first_at = demand.requests.first().map_or(Span::DUMMY, |r| r.at);
    // One key per generator module (by its file), and one host per directory the generator
    // modules are imported from.
    let mut files: Vec<PathBuf> = vec![];
    let mut fn_keys: Vec<String> = vec![];
    let mut hosts: Vec<HostPlan> = vec![];
    // Only the generator modules of requested functions run (with every function of theirs: a
    // generator may ask for instances of the module's other functions).
    let requested: Vec<(&str, &str)> = (demand.requests.iter())
        .map(|r| &demand.functions[r.function])
        .map(|f| (f.module.as_str(), f.generator.as_str()))
        .collect();
    for f in &demand.functions {
        let decl = modules.iter().find(|m| m.path == f.module);
        let Some(decl) = decl.filter(|_| requested.contains(&(&f.module, &f.generator))) else {
            fn_keys.push(String::new());
            continue;
        };
        // Absolute: the host's root is put beside it, and the caches key hosts by that root, so
        // two projects' `main.vlt` run from their own directories never share a host.
        let decl_path = vpm::relpath::absolute(&sess.sm.get(decl.file).path);
        let Some((file, dir, spec)) = locate_generator(&decl_path, &f.generator, packages) else {
            let msg = format!("cannot find the generator module `{}`", f.generator);
            sess.diagnostics
                .push(Diagnostic::error(msg, f.generator_span));
            return None;
        };
        let i = files.iter().position(|k| *k == file).unwrap_or_else(|| {
            files.push(file);
            files.len() - 1
        });
        let key = format!("g{i}");
        let h = hosts.iter().position(|h| h.dir == dir).unwrap_or_else(|| {
            hosts.push(HostPlan {
                dir,
                imports: vec![],
                entries: vec![],
            });
            hosts.len() - 1
        });
        let host = &mut hosts[h];
        if !host.imports.iter().any(|(k, _)| *k == key) {
            host.imports.push((key.clone(), spec));
        }
        if !host.entries.contains(&(key.clone(), f.function.clone())) {
            host.entries.push((key.clone(), f.function.clone()));
        }
        fn_keys.push(key);
    }
    let prefix = format!("__gen_r{round}_");
    let request_names: Vec<String> = (demand.requests.iter().enumerate())
        .map(|(i, r)| format!("{prefix}{i}_{}", demand.functions[r.function].function))
        .collect();
    let types = model_json(&demand.model);
    let functions: Vec<Value> = (demand.functions.iter().zip(&fn_keys))
        .map(|(f, key)| {
            json!({
                "generator": key,
                "function": f.function,
                "params": f.params.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
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
    let mut outputs = vec![];
    let mut generator_files: Vec<String> = vec![];
    for host in &hosts {
        let requests: Vec<Value> = (demand.requests.iter().zip(&request_names))
            .filter(|(r, _)| host.imports.iter().any(|(k, _)| *k == fn_keys[r.function]))
            .map(|(r, name)| {
                json!({
                    "name": name,
                    "generator": fn_keys[r.function],
                    "function": demand.functions[r.function].function,
                    "typeArgs": r.type_args,
                })
            })
            .collect();
        let Some(first) = (demand.requests.iter())
            .find(|r| host.imports.iter().any(|(k, _)| *k == fn_keys[r.function]))
        else {
            continue;
        };
        let input = json!({
            "prefix": prefix,
            "types": types,
            "functions": functions,
            "requests": requests,
        })
        .to_string();
        let src = host.source();
        let at = first.at;
        let generator = &demand.functions[first.function].generator;
        let (out, files) = run_host(sess, host, &src, &input, packages, (generator, at))?;
        outputs.push(out);
        for f in files {
            if !generator_files.contains(&f) {
                generator_files.push(f);
            }
        }
    }
    let mut errors: Diagnostics = vec![];
    for output in &outputs {
        if let Err(msg) = parse_frames(output, &mut written, &mut errors, demand, first_at) {
            let d = Diagnostic::error("a generator's output could not be read", first_at)
                .with_note(msg);
            sess.diagnostics.push(d);
            return None;
        }
    }
    if !errors.is_empty() {
        // An instance a generator refused is asked for again by each later round: one report.
        for e in errors {
            let primary = |d: &Diagnostic| d.labels.first().map(|l| l.span);
            let same = |d: &Diagnostic| {
                d.message == e.message && d.notes == e.notes && primary(d) == primary(&e)
            };
            if !sess.diagnostics.iter().any(same) {
                sess.diagnostics.push(e);
            }
        }
        return None;
    }
    // The code, per declaring module.
    let mut modules_out: Vec<(String, String)> = vec![];
    let mut accesses: Vec<(String, Vec<String>)> = vec![];
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
        let displays: Vec<&str> = (w.args.iter())
            .map(|a| demand.model.types[*a as usize].display.as_str())
            .collect();
        let mut text = format!(
            "\n// {} for {}<{}>\n",
            f.generator,
            f.function,
            displays.join(", ")
        );
        let type_args: Vec<String> = spellings.iter().map(|s| s.to_string()).collect();
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
        let desc = format!(
            "`{}<{}>` by `{}`",
            f.function,
            displays.join(", "),
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
        // A requested instance is found again by its key; one a generator asked for, with
        // the module of the instance that asked.
        let fingerprint = (demand.requests.iter().zip(&request_names))
            .find(|(_, n)| *n == name)
            .map_or(String::new(), |(r, _)| r.fingerprint.clone());
        instances.push(GenInstance {
            module: f.module.clone(),
            function: f.function.clone(),
            type_args,
            body: name.clone(),
            // Set by the driver, which names the generated module.
            body_module: String::new(),
            fingerprint,
        });
        let access = reachable_types(&demand.model, &w.args);
        match accesses.iter_mut().find(|(m, _)| *m == f.module) {
            Some((_, a)) => {
                for q in access {
                    if !a.contains(&q) {
                        a.push(q);
                    }
                }
            }
            None => accesses.push((f.module.clone(), access)),
        }
    }
    Some(Generated {
        modules: (modules_out.into_iter())
            .map(|(declaring, text)| {
                let access = (accesses.iter().find(|(m, _)| *m == declaring))
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default();
                GeneratedText {
                    declaring,
                    text,
                    access,
                }
            })
            .collect(),
        instances,
        described,
        generator_files,
    })
}

/// The qualified names of the declared classes and structs that model types `roots` reach:
/// the types whose access code generated for them has.
fn reachable_types(model: &velt_sema::generated::model::Model, roots: &[u32]) -> Vec<String> {
    let mut seen: Vec<u32> = roots.to_vec();
    let mut out = vec![];
    let mut next = 0;
    while next < seen.len() {
        let Some(t) = model.types.get(seen[next] as usize) else {
            next += 1;
            continue;
        };
        next += 1;
        if matches!(t.kind, "class" | "struct") && !t.qualified_name.is_empty() {
            out.push(t.qualified_name.clone());
        }
        let refs = (t.args.iter().chain(&t.arms).chain(&t.subclasses))
            .copied()
            .chain(t.base)
            .chain(t.fields.iter().map(|f| f.ty));
        for r in refs {
            if !seen.contains(&r) {
                seen.push(r);
            }
        }
    }
    out
}

/// One host program: the generator modules it imports (`(key, specifier)`) from a virtual root
/// in `dir`, and the generated functions it serves (`(key, function)`).
struct HostPlan {
    dir: PathBuf,
    imports: Vec<(String, String)>,
    entries: Vec<(String, String)>,
}

impl HostPlan {
    fn source(&self) -> String {
        let mut src = String::from("import { serve } from \"velt/generate\";\n");
        for (key, spec) in &self.imports {
            src.push_str(&format!("import * as {key} from {spec:?};\n"));
        }
        src.push_str("\nfunction main() {\n  serve([\n");
        for (key, function) in &self.entries {
            src.push_str(&format!(
                "    {{ key: {key:?}, fn: {function:?}, run: {key}.{function} }},\n"
            ));
        }
        src.push_str("  ]);\n}\n");
        src
    }
}

/// The generator module `spec` names, resolved like an import of the declaring module at
/// `decl` (a relative path, a package's module or a std module): its file, the directory its
/// host's root is put in and the specifier the host imports it by. The host's root sits beside
/// the declaring module, so it imports the generator by the same specifier; a std generator is
/// imported as the std module it is (`velt/…`).
fn locate_generator(
    decl: &Path,
    spec: &str,
    packages: Option<&vpm::PackageGraph>,
) -> Option<(PathBuf, PathBuf, String)> {
    let std_root = crate::loader::std_root();
    let resolver = packages.map(|g| g as &dyn crate::loader::PackageResolver);
    let file = crate::loader::resolve_module(spec, decl, std_root.as_deref(), resolver)?;
    if let Some(std) = std_root
        .as_deref()
        .filter(|s| crate::loader::in_std(&file, s))
    {
        let rel = vpm::relpath::relative(&file, std);
        let module = crate::loader::module_path(&rel).to_string();
        return Some((file, std.to_path_buf(), format!("velt/{module}")));
    }
    Some((file, decl.parent()?.to_path_buf(), spec.to_string()))
}

/// Run the host `plan` (source `src`) on `input`, from the output cache when it has the
/// answer: its output and the files its build read. `None` with the error in `sess`
/// (`failing`: the generator to blame and the call that needed it).
fn run_host(
    sess: &mut Session,
    plan: &HostPlan,
    src: &str,
    input: &str,
    packages: Option<&vpm::PackageGraph>,
    failing: (&str, Span),
) -> Option<(Vec<u8>, Vec<String>)> {
    let (generator, at) = failing;
    let host_root = plan.dir.join("__velt_generate_host.vlt");
    let key = cache::output_key(&host_root, src, input);
    if let Some(hit) = key.as_ref().and_then(cache::cached_output) {
        return Some(hit);
    }
    if super::guard::building(sess.generator_build) {
        let msg = "generators need generated code to be built, in a cycle";
        sess.diagnostics.push(Diagnostic::error(msg, at));
        return None;
    }
    let ran = host::prepare(&host_root, src, packages).and_then(|h| {
        let files = h.files.clone();
        h.run(input).map(|out| (out, files))
    });
    match ran {
        Ok((out, files)) => {
            if let Some(key) = &key {
                cache::store_output(key, &out, &files);
            }
            Some((out, files))
        }
        Err(msg) => {
            let d =
                Diagnostic::error(format!("the generator `{generator}` failed"), at).with_note(msg);
            sess.diagnostics.push(d);
            None
        }
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
                        "isGetter": f.getter,
                        "stdPrivate": f.std_private,
                        "via": f.via.map_or(-1, i64::from),
                        "at": f.at,
                    })).collect::<Vec<_>>(),
                    "base": opt(t.base),
                    "subclasses": t.subclasses,
                    "members": t.members.iter().map(|m| json!({
                        "name": m.name,
                        "value": m.value,
                    })).collect::<Vec<_>>(),
                    "literal": t.literal,
                    "mayBeProxy": t.may_be_proxy,
                    "hasProperties": t.has_properties,
                    "classValue": t.class_value,
                    "dynamic": t.dynamic,
                    "privateField": t.private_field,
                    "ctorAccess": t.ctor_access,
                    "methods": t.methods.iter().map(|m| json!({
                        "name": m.name,
                        "params": m.params,
                        "ret": m.ret,
                    })).collect::<Vec<_>>(),
                    "at": opt(t.at),
                })
            })
            .collect(),
    )
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
            ["files"] => {}
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
                // The first line is the message, each further line a note.
                let mut lines = text.lines();
                let mut d = Diagnostic::error(lines.next().unwrap_or(""), span);
                for note in lines.filter(|l| !l.is_empty()) {
                    d = d.with_note(note);
                }
                errors.push(d);
            }
            _ => return Err(format!("an unknown record `#{header}`")),
        }
    }
    if !done {
        return Err("the generator host stopped before it finished".into());
    }
    Ok(())
}
