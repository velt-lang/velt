//! Writing a [`Manifest`] as `package.velt` text (scaffolding, the `velt.toml` migration) and as
//! JSON (`velt manifest --json`).

use serde_json::{json, Map, Value};

use super::{Dependency, Manifest, DEFAULT_ENTRY, DEFAULT_NATIVE_PATH};

/// The first line of every generated manifest.
pub(crate) const TYPES_IMPORT: &str = "import type { Package } from \"velt/package\";";

pub(super) fn to_velt(m: &Manifest) -> String {
    let mut fields = vec![
        format!("name: {}", string_lit(&m.package.name)),
        format!("version: {}", string_lit(&m.package.version)),
    ];
    if let Some(toolchain) = &m.toolchain {
        fields.push(format!("velt: {}", string_lit(toolchain)));
    }
    if let Some(description) = &m.package.description {
        fields.push(format!("description: {}", string_lit(description)));
    }
    if !m.package.keywords.is_empty() {
        let words: Vec<String> = m.package.keywords.iter().map(|k| string_lit(k)).collect();
        fields.push(format!("keywords: [{}]", words.join(", ")));
    }
    if m.package.entry != DEFAULT_ENTRY {
        fields.push(format!("entry: {}", string_lit(&m.package.entry)));
    }
    if let Some(registry) = &m.registry {
        fields.push(format!("registry: {}", string_lit(registry)));
    }
    if !m.dependencies.is_empty() {
        let deps = m
            .dependencies
            .iter()
            .map(|(name, dep)| format!("{}: {}", key(name), dependency_lit(dep)));
        fields.push(format!("dependencies: {}", multiline_object(deps)));
    }
    if !m.paths.is_empty() {
        let paths = m
            .paths
            .iter()
            .map(|(pattern, target)| format!("{}: {}", key(pattern), string_lit(target)));
        fields.push(format!("paths: {}", multiline_object(paths)));
    }
    if !m.imports.is_empty() {
        let imports = m.imports.iter().map(|(name, variants)| {
            let conditions = variants
                .iter()
                .map(|v| format!("{}: {}", key(&v.condition), string_lit(&v.target)));
            format!("{}: {}", key(name), inline_object(conditions))
        });
        fields.push(format!("imports: {}", multiline_object(imports)));
    }
    if !m.host_modules.is_empty() {
        let modules = m
            .host_modules
            .iter()
            .map(|(name, file)| format!("{}: {}", key(name), string_lit(file)));
        fields.push(format!("hostModules: {}", multiline_object(modules)));
    }
    if let Some(host) = &m.host {
        let mut parts = vec![
            format!("target: {}", string_lit(&host.target)),
            format!("condition: {}", string_lit(&host.condition)),
            format!("entry: {}", string_lit(&host.entry)),
        ];
        if !host.scaffold.is_empty() {
            let files = host
                .scaffold
                .iter()
                .map(|(path, file)| format!("{}: {}", key(path), string_lit(file)));
            parts.push(format!("scaffold: {}", inline_object(files)));
        }
        fields.push(format!("host: {}", inline_object(parts.into_iter())));
    }
    if let Some(jsx) = &m.jsx {
        let source = jsx
            .import_source
            .iter()
            .map(|s| format!("importSource: {}", string_lit(s)));
        fields.push(format!("jsx: {}", inline_object(source)));
    }
    if !m.define.is_empty() {
        let defines = m
            .define
            .iter()
            .map(|(name, value)| format!("{}: {}", key(name), string_lit(value)));
        fields.push(format!("define: {}", multiline_object(defines)));
    }
    if m.native.is_some() || m.c_libraries.is_some() {
        let mut parts = Vec::new();
        if let Some(native) = &m.native {
            if native.path != DEFAULT_NATIVE_PATH {
                parts.push(format!("path: {}", string_lit(&native.path)));
            }
            if !native.targets.is_empty() {
                let targets: Vec<String> = native.targets.iter().map(|t| string_lit(t)).collect();
                parts.push(format!("targets: [{}]", targets.join(", ")));
            }
            if native.wasm {
                parts.push("wasm: true".into());
            }
        }
        if !m.c_targets.is_empty() {
            let targets: Vec<String> = m.c_targets.iter().map(|t| string_lit(t)).collect();
            parts.push(format!("targets: [{}]", targets.join(", ")));
        }
        if let Some(libraries) = &m.c_libraries {
            let libs = libraries.iter().map(|(name, lib)| {
                let system = lib
                    .system
                    .iter()
                    .map(|(cond, s)| format!("{}: {}", key(cond), string_lit(s)));
                let mut fields: Vec<String> = Vec::new();
                if !lib.system.is_empty() {
                    fields.push(format!("system: {}", inline_object(system)));
                }
                if !lib.headers.is_empty() {
                    let headers: Vec<String> = lib.headers.iter().map(|h| string_lit(h)).collect();
                    fields.push(format!("headers: [{}]", headers.join(", ")));
                }
                if lib.unchecked {
                    fields.push("unchecked: true".into());
                }
                if !lib.sources.is_empty() {
                    let sources: Vec<String> = lib.sources.iter().map(|s| string_lit(s)).collect();
                    fields.push(format!("sources: [{}]", sources.join(", ")));
                }
                if !lib.defines.is_empty() {
                    let defines = lib
                        .defines
                        .iter()
                        .map(|(name, value)| format!("{}: {}", key(name), string_lit(value)));
                    fields.push(format!("defines: {}", inline_object(defines)));
                }
                format!("{}: {}", key(name), inline_object(fields.into_iter()))
            });
            parts.push(format!("c: {}", inline_object(libs)));
        }
        fields.push(format!("native: {}", inline_object(parts.into_iter())));
    }
    if !m.allow_native.is_empty() {
        let names: Vec<String> = m.allow_native.iter().map(|n| n.to_source()).collect();
        fields.push(format!("allowNative: [{}]", names.join(", ")));
    }
    if !m.ts_compat.is_empty() {
        let dirs: Vec<String> = m.ts_compat.iter().map(|d| string_lit(d)).collect();
        fields.push(format!("tsCompat: [{}]", dirs.join(", ")));
    }
    if let Some(lint) = &m.lint {
        fields.push(format!("lint: {}", lint_velt(lint)));
    }
    if let Some(build) = &m.build {
        let cpu = build.cpu.iter().map(|c| format!("cpu: {}", string_lit(c)));
        fields.push(format!("build: {}", inline_object(cpu)));
    }
    let text = format!(
        "{TYPES_IMPORT}\n\nexport const pkg: Package = {};\n",
        multiline_object(fields.into_iter())
    );
    velt_fmt::format_source(&text).expect("ICE: a generated manifest is valid Velt")
}

pub(super) fn to_json(m: &Manifest) -> Value {
    let mut out = Map::new();
    out.insert("name".into(), json!(m.package.name));
    out.insert("version".into(), json!(m.package.version));
    if let Some(toolchain) = &m.toolchain {
        out.insert("velt".into(), json!(toolchain));
    }
    if let Some(description) = &m.package.description {
        out.insert("description".into(), json!(description));
    }
    if !m.package.keywords.is_empty() {
        out.insert("keywords".into(), json!(m.package.keywords));
    }
    out.insert("entry".into(), json!(m.package.entry));
    if let Some(registry) = &m.registry {
        out.insert("registry".into(), json!(registry));
    }
    let deps: Map<String, Value> = m
        .dependencies
        .iter()
        .map(|(name, dep)| {
            let value = match dep {
                Dependency::Version(req) => json!(req),
                Dependency::Detailed(d) => {
                    let mut o = Map::new();
                    if let Some(v) = &d.version {
                        o.insert("version".into(), json!(v));
                    }
                    for (key, value) in [
                        ("path", &d.path),
                        ("git", &d.git),
                        ("branch", &d.branch),
                        ("tag", &d.tag),
                        ("rev", &d.rev),
                        ("url", &d.url),
                        ("sha256", &d.sha256),
                    ] {
                        if let Some(v) = value {
                            o.insert(key.into(), json!(v));
                        }
                    }
                    if let Some(n) = d.strip {
                        o.insert("strip".into(), json!(n));
                    }
                    Value::Object(o)
                }
            };
            (name.clone(), value)
        })
        .collect();
    out.insert("dependencies".into(), Value::Object(deps));
    out.insert("paths".into(), json!(m.paths));
    if !m.imports.is_empty() {
        // An object's keys keep their order in `serde_json` only with `preserve_order`: the
        // variants are a list of `[condition, file]` pairs, in the order written.
        let imports: Map<String, Value> = m
            .imports
            .iter()
            .map(|(name, variants)| {
                let pairs: Vec<Value> = variants
                    .iter()
                    .map(|v| json!([v.condition, v.target]))
                    .collect();
                (name.clone(), Value::Array(pairs))
            })
            .collect();
        out.insert("imports".into(), Value::Object(imports));
    }
    if !m.host_modules.is_empty() {
        out.insert("hostModules".into(), json!(m.host_modules));
    }
    if let Some(host) = &m.host {
        out.insert(
            "host".into(),
            json!({
                "target": host.target,
                "condition": host.condition,
                "entry": host.entry,
                "scaffold": host.scaffold,
            }),
        );
    }
    if let Some(jsx) = &m.jsx {
        let mut o = Map::new();
        if let Some(source) = &jsx.import_source {
            o.insert("importSource".into(), json!(source));
        }
        out.insert("jsx".into(), Value::Object(o));
    }
    if !m.define.is_empty() {
        out.insert("define".into(), json!(m.define));
    }
    if m.native.is_some() || m.c_libraries.is_some() {
        let mut o = match &m.native {
            Some(native) => {
                let crate_fields =
                    json!({ "path": native.path, "targets": native.targets, "wasm": native.wasm });
                crate_fields.as_object().cloned().unwrap_or_default()
            }
            None => Map::new(),
        };
        if !m.c_targets.is_empty() {
            o.insert("targets".into(), json!(m.c_targets));
        }
        if let Some(libraries) = &m.c_libraries {
            let libs: Map<String, Value> = libraries
                .iter()
                .map(|(name, lib)| {
                    let system: Map<String, Value> = lib
                        .system
                        .iter()
                        .map(|(c, s)| (c.clone(), json!(s)))
                        .collect();
                    let mut entry = json!({});
                    if !system.is_empty() {
                        entry["system"] = json!(system);
                    }
                    if !lib.headers.is_empty() {
                        entry["headers"] = json!(lib.headers);
                    }
                    if lib.unchecked {
                        entry["unchecked"] = json!(true);
                    }
                    if !lib.sources.is_empty() {
                        entry["sources"] = json!(lib.sources);
                    }
                    if !lib.defines.is_empty() {
                        let defines: Map<String, Value> = lib
                            .defines
                            .iter()
                            .map(|(name, value)| (name.clone(), json!(value)))
                            .collect();
                        entry["defines"] = json!(defines);
                    }
                    (name.clone(), entry)
                })
                .collect();
            o.insert("c".into(), Value::Object(libs));
        }
        out.insert("native".into(), Value::Object(o));
    }
    if !m.allow_native.is_empty() {
        let entries = m.allow_native.iter().map(|n| n.to_json()).collect();
        out.insert("allowNative".into(), Value::Array(entries));
    }
    if !m.ts_compat.is_empty() {
        out.insert("tsCompat".into(), json!(m.ts_compat));
    }
    if let Some(lint) = &m.lint {
        let rules: Map<String, Value> = lint
            .rules
            .iter()
            .map(|(code, level)| (code.clone(), json!(level.name())))
            .collect();
        let mut o = Map::new();
        if let Some(perf) = lint.perf {
            o.insert("perf".into(), json!(perf.name()));
        }
        if !rules.is_empty() {
            o.insert("rules".into(), Value::Object(rules));
        }
        out.insert("lint".into(), Value::Object(o));
    }
    if let Some(build) = &m.build {
        let mut o = Map::new();
        if let Some(cpu) = &build.cpu {
            o.insert("cpu".into(), json!(cpu));
        }
        out.insert("build".into(), Value::Object(o));
    }
    Value::Object(out)
}

/// `{ … }` with one property per line (`velt fmt` joins it when it fits on one line).
fn multiline_object(props: impl Iterator<Item = String>) -> String {
    let body: String = props.map(|p| format!("  {p},\n")).collect();
    format!("{{\n{body}}}")
}

/// `{ a: x, b: y }` on one line (`velt fmt` breaks it when it is too long).
fn inline_object(props: impl Iterator<Item = String>) -> String {
    let props: Vec<String> = props.collect();
    if props.is_empty() {
        "{}".into()
    } else {
        format!("{{ {} }}", props.join(", "))
    }
}

/// A dependency's value: `"1.2"` or `{ version: "1.2", path: "../x" }`.
pub(crate) fn dependency_lit(dep: &Dependency) -> String {
    match dep {
        Dependency::Version(req) => string_lit(req),
        Dependency::Detailed(d) => {
            let version = d
                .version
                .iter()
                .map(|v| format!("version: {}", string_lit(v)));
            let rest = [
                ("git", &d.git),
                ("branch", &d.branch),
                ("tag", &d.tag),
                ("rev", &d.rev),
                ("url", &d.url),
                ("sha256", &d.sha256),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.as_ref().map(|v| format!("{key}: {}", string_lit(v))));
            let strip = d.strip.iter().map(|n| format!("strip: {n}"));
            let path = d.path.iter().map(|p| format!("path: {}", string_lit(p)));
            inline_object(version.chain(rest).chain(strip).chain(path))
        }
    }
}

/// An object key: bare when it is an identifier, quoted otherwise (`"my-lib"`, `"@app/*"`).
pub(crate) fn key(name: &str) -> String {
    let mut chars = name.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ident {
        name.to_string()
    } else {
        string_lit(name)
    }
}

/// A double-quoted Velt string literal.
pub(crate) fn string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `lint` as `package.velt` writes it: `{ perf: "warn", rules: { "perf/x": "off" } }`.
fn lint_velt(lint: &super::LintConfig) -> String {
    let mut parts = vec![];
    if let Some(perf) = lint.perf {
        parts.push(format!("perf: {}", string_lit(perf.name())));
    }
    if !lint.rules.is_empty() {
        let rules = lint
            .rules
            .iter()
            .map(|(code, level)| format!("{}: {}", string_lit(code), string_lit(level.name())));
        parts.push(format!("rules: {}", inline_object(rules)));
    }
    inline_object(parts.into_iter())
}
