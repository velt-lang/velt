//! Writing a [`Manifest`] as `package.vlt` text (scaffolding, the `velt.toml` migration) and as
//! JSON (`velt manifest --json`).

use serde_json::{json, Map, Value};

use super::{Dependency, Manifest, DEFAULT_ENTRY, DEFAULT_NATIVE_PATH};

/// The first line of every generated manifest.
pub(crate) const TYPES_IMPORT: &str = "import type { Package } from \"velt:package\";";

pub(super) fn to_vlt(m: &Manifest) -> String {
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
    if let Some(jsx) = &m.jsx {
        let source = jsx
            .import_source
            .iter()
            .map(|s| format!("importSource: {}", string_lit(s)));
        fields.push(format!("jsx: {}", inline_object(source)));
    }
    if let Some(native) = &m.native {
        let mut parts = Vec::new();
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
        fields.push(format!("native: {}", inline_object(parts.into_iter())));
    }
    if !m.ts_compat.is_empty() {
        let dirs: Vec<String> = m.ts_compat.iter().map(|d| string_lit(d)).collect();
        fields.push(format!("tsCompat: [{}]", dirs.join(", ")));
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
                    if let Some(p) = &d.path {
                        o.insert("path".into(), json!(p));
                    }
                    Value::Object(o)
                }
            };
            (name.clone(), value)
        })
        .collect();
    out.insert("dependencies".into(), Value::Object(deps));
    out.insert("paths".into(), json!(m.paths));
    if let Some(jsx) = &m.jsx {
        let mut o = Map::new();
        if let Some(source) = &jsx.import_source {
            o.insert("importSource".into(), json!(source));
        }
        out.insert("jsx".into(), Value::Object(o));
    }
    if let Some(native) = &m.native {
        out.insert(
            "native".into(),
            json!({ "path": native.path, "targets": native.targets, "wasm": native.wasm }),
        );
    }
    if !m.ts_compat.is_empty() {
        out.insert("tsCompat".into(), json!(m.ts_compat));
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
            let path = d.path.iter().map(|p| format!("path: {}", string_lit(p)));
            inline_object(version.chain(path))
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
