//! One round of generating: the host program for the generators a demand names, its input
//! (the requests and the type model), its output read back into generated modules.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use velt_common::{Diagnostic, Diagnostics, Span};
use velt_sema::generated::{spell, Demand, GenInstance};
use velt_sema::SourceModule;

use super::{cache, host, Described, Generated};
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
) -> Option<Generated> {
    let first_at = demand.requests.first().map_or(Span::DUMMY, |r| r.at);
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
    let input = input.to_string();
    let failed = |sess: &mut Session, msg: String| {
        let f = &demand.functions[demand.requests[0].function];
        let d = Diagnostic::error(format!("the generator `{}` failed", f.generator), first_at)
            .with_note(msg);
        sess.diagnostics.push(d);
    };
    let key = cache::output_key(&src, &input);
    let (output, generator_files) = match key.as_ref().and_then(cache::cached_output) {
        Some(hit) => hit,
        None => {
            if super::guard::building(&src) {
                let msg = "generators need generated code to be built, in a cycle";
                sess.diagnostics.push(Diagnostic::error(msg, first_at));
                return None;
            }
            let ran = super::guard::with(&src, || {
                host::prepare(&host_root, &src).and_then(|h| {
                    let files = h.files.clone();
                    h.run(&input).map(|out| (out, files))
                })
            });
            match ran {
                Ok((out, files)) => {
                    if let Some(key) = &key {
                        cache::store_output(key, &out, &files);
                    }
                    (out, files)
                }
                Err(msg) => {
                    failed(sess, msg);
                    return None;
                }
            }
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
    let mut fingerprints = vec![];
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
        fingerprints.push(demand.model.fingerprint(&w.args, &f.signature()));
        instances.push(GenInstance {
            module: f.module.clone(),
            function: f.function.clone(),
            type_args,
            body: name.clone(),
            fingerprint: String::new(),
        });
    }
    Some(Generated {
        modules: modules_out,
        instances,
        fingerprints,
        described,
        generator_files,
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
