//! `velt doc --format json`: the output for a fixture package that has every kind of export,
//! members, tags, links and re-exports (`tests/json/pkg`, snapshot `tests/json/pkg.api.json`;
//! `VELT_BLESS=1` rewrites it), and for the whole standard library: valid JSON for every module,
//! and every link leads to a documented item.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

fn dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/json"))
}

/// Whether a JSON `kind` is a type's (a type and a value may share a name).
fn is_type(kind: &Value) -> bool {
    ["class", "struct", "interface", "enum", "type"]
        .iter()
        .any(|k| kind == *k)
}

/// The exports each module declares itself (not re-exported, and not `extend` blocks, which
/// share their type's name), by module, name and whether it is a type.
fn declared(doc: &Value) -> HashMap<(String, String, bool), &Value> {
    let mut out = HashMap::new();
    for m in doc["modules"].as_array().expect("modules") {
        let module = m["module"].as_str().expect("module").to_string();
        for e in m["exports"].as_array().expect("exports") {
            if e["reexportedFrom"].is_null() && e["kind"] != "extend" {
                let name = e["name"].as_str().expect("name").to_string();
                out.insert((module.clone(), name, is_type(&e["kind"])), e);
            }
        }
    }
    out
}

/// Every link target in `doc` (in signatures and doc comments) names an item a module
/// declares, with that kind (and a member it has); spans cover an identifier of the
/// signature. Returns the problems, and the `{@link}`s that lead nowhere.
fn check_links(doc: &Value) -> (Vec<String>, Vec<String>) {
    let declared = declared(doc);
    let mut broken = vec![];
    let mut unresolved = vec![];
    let check = |at: &str, t: &Value, broken: &mut Vec<String>| {
        let key = (
            t["module"].as_str().unwrap_or("").to_string(),
            t["name"].as_str().unwrap_or("").to_string(),
            is_type(&t["kind"]),
        );
        let Some(item) = declared.get(&key) else {
            broken.push(format!("{at}: no `{}` in `{}`", key.1, key.0));
            return;
        };
        if item["kind"] != t["kind"] {
            broken.push(format!(
                "{at}: `{}` is a {}, not {}",
                key.1, item["kind"], t["kind"]
            ));
        }
        let id = match t["member"].as_str() {
            Some(member) => {
                let members = item["members"].as_array().expect("members");
                match members.iter().find(|m| m["name"] == member) {
                    Some(m) => &m["id"],
                    None => {
                        broken.push(format!("{at}: `{}` has no member `{member}`", key.1));
                        return;
                    }
                }
            }
            None => &item["id"],
        };
        if *id != t["id"] {
            broken.push(format!(
                "{at}: target id {} is not the item's {id}",
                t["id"]
            ));
        }
    };
    fn walk<'a>(at: String, item: &'a Value, out: &mut Vec<(String, &'a Value)>) {
        for m in item["members"].as_array().into_iter().flatten() {
            walk(
                format!("{at}.{}", m["name"].as_str().unwrap_or("?")),
                m,
                out,
            );
        }
        out.push((at, item));
    }
    let mut ids = std::collections::HashSet::new();
    for m in doc["modules"].as_array().expect("modules") {
        let module = m["module"].as_str().expect("module");
        let mut items = vec![(module.to_string(), m)];
        for e in m["exports"].as_array().expect("exports") {
            walk(
                format!("{module}: {}", e["name"].as_str().unwrap_or("?")),
                e,
                &mut items,
            );
        }
        for (at, item) in items {
            if let Some(id) = item["id"].as_str() {
                if !ids.insert(id.to_string()) {
                    broken.push(format!("{at}: id `{id}` is not unique"));
                }
            }
            for link in item["links"].as_array().expect("links") {
                match &link["target"] {
                    Value::Null => unresolved.push(format!("{at}: {{@link {}}}", link["ref"])),
                    t => check(&at, t, &mut broken),
                }
            }
            let Some(sig) = item["signature"].as_str() else {
                continue; // a module
            };
            let units: Vec<u16> = sig.encode_utf16().collect();
            for span in item["spans"].as_array().expect("spans") {
                let (start, end) = (span["start"].as_u64(), span["end"].as_u64());
                let text = match (start, end) {
                    (Some(s), Some(e)) if s < e && e as usize <= units.len() => {
                        String::from_utf16_lossy(&units[s as usize..e as usize])
                    }
                    _ => {
                        broken.push(format!("{at}: span {span} outside `{sig}`"));
                        continue;
                    }
                };
                let ident = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.');
                if !text.chars().all(ident) {
                    broken.push(format!("{at}: span `{text}` of `{sig}` is not a name"));
                }
                let roles = [
                    "param",
                    "return",
                    "throws",
                    "extends",
                    "implements",
                    "typeArg",
                    "field",
                    "type",
                ];
                if !roles.iter().any(|r| span["role"] == *r) {
                    broken.push(format!("{at}: span `{text}` has role {}", span["role"]));
                }
                check(&at, &span["target"], &mut broken);
            }
        }
    }
    (broken, unresolved)
}

#[test]
fn fixture_package_snapshot() {
    let inputs = velt_doc::inputs_from_dir(&dir().join("pkg"), "pkg").expect("fixture");
    let json = velt_doc::api_json("pkg", &inputs);
    let doc: Value = serde_json::from_str(&json).expect("valid JSON");
    let (broken, unresolved) = check_links(&doc);
    assert!(broken.is_empty(), "{}", broken.join("\n"));
    assert!(unresolved.is_empty(), "{}", unresolved.join("\n"));

    let snapshot = dir().join("pkg.api.json");
    // Line endings may be CRLF in a Windows checkout.
    let want = std::fs::read_to_string(&snapshot)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if std::env::var_os("VELT_BLESS").is_some() {
        std::fs::write(&snapshot, &json).expect("write snapshot");
    } else {
        assert!(
            json == want,
            "`velt doc --format json` output of tests/json/pkg changed (VELT_BLESS=1 rewrites \
             tests/json/pkg.api.json):\n{json}"
        );
    }
}

type Pred = dyn Fn(&Value) -> bool;

fn is_target(v: &Value) -> bool {
    v.get("module").is_some() && v.get("member").is_some()
}

/// The first object in `v` (depth first) for which `pred` holds.
fn find_mut<'a>(v: &'a mut Value, pred: &Pred) -> Option<&'a mut Value> {
    if pred(v) {
        return Some(v);
    }
    match v {
        Value::Array(items) => items.iter_mut().find_map(|i| find_mut(i, pred)),
        Value::Object(fields) => fields.values_mut().find_map(|f| find_mut(f, pred)),
        _ => None,
    }
}

#[test]
fn a_broken_target_is_reported() {
    let inputs = velt_doc::inputs_from_dir(&dir().join("pkg"), "pkg").expect("fixture");
    let json = velt_doc::api_json("pkg", &inputs);
    // A span to a type that isn't there, a link to a member that isn't there, a target of the
    // wrong kind, a wrong id.
    let breaks: [(&str, &Pred, &str, &str); 4] = [
        (
            "Point",
            &|v| is_target(v) && v["name"] == "Point",
            "name",
            "Pt",
        ),
        (
            "member",
            &|v| is_target(v) && v["member"] == "length",
            "member",
            "len",
        ),
        (
            "kind",
            &|v| is_target(v) && v["name"] == "LIMIT",
            "kind",
            "type",
        ),
        (
            "id",
            &|v| is_target(v) && v["name"] == "Pair",
            "id",
            "pkg/lib#Pairs",
        ),
    ];
    for (what, pred, field, value) in breaks {
        let mut doc: Value = serde_json::from_str(&json).expect("valid JSON");
        let target = find_mut(&mut doc, pred).unwrap_or_else(|| panic!("no {what} target"));
        target[field] = Value::from(value);
        let (broken, _) = check_links(&doc);
        assert!(!broken.is_empty(), "a broken {what} is not reported");
    }
}

#[test]
fn every_std_module_serializes_and_its_links_resolve() {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let inputs = velt_doc::inputs_from_dir(&std, "std").expect("read std");
    assert!(inputs.len() > 50, "{} std modules", inputs.len());
    let json = velt_doc::api_json("Velt standard library", &inputs);
    let doc: Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["schemaVersion"], velt_doc::json::SCHEMA_VERSION);
    let modules = doc["modules"].as_array().expect("modules");
    let names: Vec<&str> = modules
        .iter()
        .filter_map(|m| m["module"].as_str())
        .collect();
    let want: Vec<&str> = inputs.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(names, want);
    let fs = modules
        .iter()
        .find(|m| m["module"] == "std/fs")
        .expect("std/fs");
    assert_eq!(fs["specifier"], "velt:fs");
    assert!(fs["exports"].as_array().is_some_and(|e| !e.is_empty()));
    let (broken, unresolved) = check_links(&doc);
    assert!(broken.is_empty(), "{}", broken.join("\n"));
    assert!(
        unresolved.is_empty(),
        "these `{{@link}}`s in std name nothing documented:\n{}",
        unresolved.join("\n")
    );
}

/// The export named `name` of kind `kind` in module `module` of `doc`.
fn export<'a>(doc: &'a Value, module: &str, name: &str, kind: &str) -> &'a Value {
    let m = doc["modules"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["module"] == module))
        .unwrap_or_else(|| panic!("no module {module}"));
    m["exports"]
        .as_array()
        .and_then(|es| es.iter().find(|e| e["name"] == name && e["kind"] == kind))
        .unwrap_or_else(|| panic!("no {kind} {name} in {module}"))
}

#[test]
fn a_type_and_a_value_with_one_name() {
    // `export type Color = …; export const Color: Color = …`: two ids, and signatures link to
    // the type, in the JSON and in the HTML pages.
    let inputs = velt_doc::inputs_from_dir(&dir().join("pkg"), "pkg").expect("fixture");
    let doc: Value = serde_json::from_str(&velt_doc::api_json("pkg", &inputs)).expect("JSON");
    assert_eq!(
        export(&doc, "pkg/lib", "Color", "type")["id"],
        "pkg/lib#type:Color"
    );
    assert_eq!(
        export(&doc, "pkg/lib", "Color", "const")["id"],
        "pkg/lib#Color"
    );
    let paint = export(&doc, "pkg/lib", "paint", "function");
    let colors: Vec<&Value> = paint["spans"]
        .as_array()
        .expect("spans")
        .iter()
        .filter(|s| s["target"]["name"] == "Color")
        .collect();
    assert_eq!(colors.len(), 2, "{}", paint["spans"]);
    for span in colors {
        assert_eq!(span["target"]["id"], "pkg/lib#type:Color");
        assert_eq!(span["target"]["kind"], "type");
    }

    let out = tempfile::tempdir().expect("tempdir");
    velt_doc::write_api_docs("pkg", "", &inputs, out.path()).expect("HTML");
    let page = std::fs::read_to_string(out.path().join("pkg.lib.html")).expect("page");
    let link = "<a href=\"#type.Color\">Color</a>";
    let sig = format!("paint(p: <a href=\"#class.Point\">Point</a>, c: {link}): {link}");
    assert!(page.contains(&sig), "no `{sig}` in the page:\n{page}");
}

#[test]
fn type_parameter_defaults_are_in_signatures() {
    let inputs = velt_doc::inputs_from_dir(&dir().join("pkg"), "pkg").expect("fixture");
    let doc: Value = serde_json::from_str(&velt_doc::api_json("pkg", &inputs)).expect("JSON");
    let first = export(&doc, "pkg/lib", "first", "function");
    let sig = first["signature"].as_str().expect("signature");
    assert!(sig.starts_with("function first<T = Point>("), "{sig}");
    let span = &first["spans"][0];
    assert_eq!(span["target"]["name"], "Point");
    assert_eq!(span["role"], "typeArg");
}

#[test]
fn span_offsets_count_utf16_code_units() {
    // `"é" | Color`: `é` is two bytes in UTF-8 and one UTF-16 code unit, so a byte count would
    // put every span after it one unit late.
    let inputs = velt_doc::inputs_from_dir(&dir().join("pkg"), "pkg").expect("fixture");
    let doc: Value = serde_json::from_str(&velt_doc::api_json("pkg", &inputs)).expect("JSON");
    let tint = export(&doc, "pkg/lib", "tint", "function");
    let sig = tint["signature"].as_str().expect("signature");
    let units: Vec<u16> = sig.encode_utf16().collect();
    let spans = tint["spans"].as_array().expect("spans");
    let names: Vec<String> = spans
        .iter()
        .map(|s| {
            let (start, end) = (s["start"].as_u64().unwrap(), s["end"].as_u64().unwrap());
            String::from_utf16_lossy(&units[start as usize..end as usize])
        })
        .collect();
    assert_eq!(names, ["Color", "Point", "Point"], "{sig}: {spans:?}");
    let at = sig.find("Color").expect("Color");
    assert_eq!(spans[0]["start"], sig[..at].encode_utf16().count());
}
