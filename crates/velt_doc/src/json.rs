//! `velt doc --format json`: the documented modules as one JSON document, for tools that render
//! API pages in their own design (the docs website: its API pages, search index and std
//! catalog). The same model as the HTML pages ([`crate::extract`], [`crate::resolve`]) with
//! the same links ([`crate::html::Links`]), but the doc comment's tags kept apart and every link
//! a target (module and name) rather than a URL. The schema is documented in
//! `docs/tooling/cli.md` ("JSON output"); bump [`SCHEMA_VERSION`] when a field changes meaning
//! or goes away (new fields don't need it).

use std::collections::{HashMap, HashSet};

use crate::comment::{DocComment, DocLink};
use crate::extract::{DocItem, DocModule, Kind};
use crate::html::{self, is_type, item_target, signature_parts, Links, Target};

/// The `schemaVersion` of the output.
pub const SCHEMA_VERSION: u64 = 1;

/// A JSON value, written with its object keys in the order given.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A non-negative integer.
    Num(u64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object.
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    fn str(s: &str) -> Json {
        Json::Str(s.to_string())
    }

    /// A string, or `null` when it is empty.
    fn text(s: &str) -> Json {
        match s.is_empty() {
            true => Json::Null,
            false => Json::str(s),
        }
    }

    fn opt(s: Option<&str>) -> Json {
        s.map_or(Json::Null, Json::str)
    }

    fn strs(items: &[String]) -> Json {
        Json::Arr(items.iter().map(|s| Json::str(s)).collect())
    }

    /// The value as JSON text, indented by two spaces, with a final newline.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        let indent = |out: &mut String, depth: usize| {
            out.push('\n');
            out.extend(std::iter::repeat_n("  ", depth));
        };
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(&n.to_string()),
            Json::Str(s) => write_string(out, s),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    item.write(out, depth + 1);
                }
                indent(out, depth);
                out.push(']');
            }
            Json::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    write_string(out, key);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The JSON document for `modules` (after [`crate::resolve`]); `files[i]` is the source file
/// of `modules[i]` as it should be shown (`std/fs.vlt`, `src/util.vlt`).
pub fn api_json(title: &str, modules: &[DocModule], files: &[String]) -> Json {
    let links = Links::new(modules);
    let file_of: HashMap<&str, &str> = modules
        .iter()
        .zip(files)
        .map(|(m, f)| (m.name.as_str(), f.as_str()))
        .collect();
    // The items declared in each module (not re-exported; an `extend` block shares its type's
    // name), for links to members: by module, name and whether it is a type (a type and a
    // value may share a name).
    let declared: HashMap<(&str, &str, bool), &DocItem> = modules
        .iter()
        .flat_map(|m| {
            m.items
                .iter()
                .filter(|it| it.origin.is_none() && it.kind != Kind::Extension)
                .map(move |it| ((m.name.as_str(), it.name.as_str(), is_type(it.kind)), it))
        })
        .collect();
    // The types whose module exports a value of the same name: their ids are `type:<name>`.
    let clash: HashSet<(&str, &str)> = modules
        .iter()
        .flat_map(|m| {
            m.items
                .iter()
                .filter(|it| shares_name_with_value(&m.items, it))
                .map(move |it| (m.name.as_str(), it.name.as_str()))
        })
        .collect();
    let ids = Ids { clash: &clash };
    let records = modules
        .iter()
        .map(|m| {
            let cx = Module {
                m,
                types: links.scope(modules, m),
                items: links.item_scope(modules, m),
                file_of: &file_of,
                declared: &declared,
                ids,
            };
            cx.record()
        })
        .collect();
    Json::Obj(vec![
        ("schemaVersion", Json::Num(SCHEMA_VERSION)),
        ("title", Json::str(title)),
        ("modules", Json::Arr(records)),
    ])
}

/// How a module is imported: `std/fs` is `velt:fs`; a package's modules by their name.
pub fn specifier(module: &str) -> String {
    match module.strip_prefix("std/") {
        Some(rest) => format!("velt:{rest}"),
        None => module.to_string(),
    }
}

/// What one module's record needs.
struct Module<'a> {
    m: &'a DocModule,
    /// Type names its signatures can refer to ([`Links::scope`]).
    types: HashMap<String, Target>,
    /// Names its `{@link}`s can refer to ([`Links::item_scope`]).
    items: HashMap<String, Target>,
    file_of: &'a HashMap<&'a str, &'a str>,
    declared: &'a HashMap<(&'a str, &'a str, bool), &'a DocItem>,
    ids: Ids<'a>,
}

/// How link targets get their `id`s.
#[derive(Clone, Copy)]
struct Ids<'a> {
    /// The types whose module exports a value of the same name, by module and name.
    clash: &'a HashSet<(&'a str, &'a str)>,
}

impl Module<'_> {
    fn record(&self) -> Json {
        let m = self.m;
        Json::Obj(vec![
            ("module", Json::str(&m.name)),
            ("specifier", Json::str(&specifier(&m.name))),
            (
                "file",
                Json::opt(self.file_of.get(m.name.as_str()).copied()),
            ),
            ("doc", Json::text(&m.doc)),
            ("summary", Json::text(&html::summary(&m.doc))),
            ("links", self.links(&m.links)),
            (
                "exports",
                Json::Arr(
                    m.items
                        .iter()
                        .enumerate()
                        .map(|(i, it)| {
                            // A re-exported item's source is its declaration's.
                            let module = it.origin.as_ref().map_or(&m.name, |o| &o.module);
                            let id = format!("{}#{}", specifier(&m.name), key(&m.items, i));
                            self.item(it, None, module, id)
                        })
                        .collect(),
                ),
            ),
        ])
    }

    /// An export, or a member of `parent`, declared in `module`, with its `id`.
    fn item(&self, item: &DocItem, parent: Option<&DocItem>, module: &str, id: String) -> Json {
        let generics: Vec<&str> = item
            .generics
            .iter()
            .chain(parent.iter().flat_map(|p| &p.generics))
            .map(String::as_str)
            .collect();
        let self_href = match parent {
            None => item_target(self.m, item).href(),
            Some(_) => String::new(),
        };
        let (kind, modifiers) = kind_and_modifiers(item);
        let source = match (self.file_of.get(module), item.line) {
            (Some(file), line) if line > 0 => Json::Obj(vec![
                ("file", Json::str(file)),
                ("line", Json::Num(line.into())),
            ]),
            _ => Json::Null,
        };
        let reexported = match &item.origin {
            Some(o) => Json::Obj(vec![
                ("module", Json::str(&o.module)),
                ("name", Json::str(&o.name)),
            ]),
            None => Json::Null,
        };
        let c = &item.comment;
        Json::Obj(vec![
            ("id", Json::str(&id)),
            ("name", Json::str(&item.name)),
            ("kind", Json::str(kind)),
            ("modifiers", Json::Arr(modifiers)),
            ("signature", Json::str(&item.signature)),
            (
                "spans",
                spans(
                    &item.signature,
                    item.kind == Kind::Field,
                    &generics,
                    &self.types,
                    &self_href,
                    self.ids,
                ),
            ),
            ("doc", Json::text(&c.body)),
            ("summary", Json::text(&html::summary(&c.body))),
            ("tags", tags(c)),
            ("links", self.links(&c.links)),
            ("source", source),
            ("reexportedFrom", reexported),
            (
                "members",
                Json::Arr(
                    item.members
                        .iter()
                        .map(|member| {
                            let id = format!("{id}.{}", member_key(item, member));
                            self.item(member, Some(item), module, id)
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn links(&self, links: &[DocLink]) -> Json {
        Json::Arr(
            links
                .iter()
                .map(|l| {
                    Json::Obj(vec![
                        ("text", Json::str(&l.text)),
                        ("ref", Json::str(&l.target)),
                        ("target", self.resolve_link(&l.target)),
                    ])
                })
                .collect(),
        )
    }

    /// What `{@link name}` refers to: an item the module can name (`readFile`, `fs.readFile`
    /// through `import * as fs`), or a member of one (`Point.len`, `Point#len`); `null` when
    /// nothing documented has that name.
    fn resolve_link(&self, name: &str) -> Json {
        let name = name.trim_end_matches("()").replace('#', ".");
        if let Some(t) = self.items.get(&name) {
            return target(t, None, self.ids);
        }
        let Some((head, member)) = name.rsplit_once('.') else {
            return Json::Null;
        };
        let Some(t) = self.items.get(head) else {
            return Json::Null;
        };
        let key = (t.module.as_str(), t.name.as_str(), is_type(t.kind));
        let Some(parent) = self.declared.get(&key) else {
            return Json::Null;
        };
        match parent.members.iter().find(|m| m.name == member) {
            Some(m) => target(t, Some((parent, m)), self.ids),
            None => Json::Null,
        }
    }
}

/// The part of an `id` after `#` for export `i` of `items`: its name; `type:<name>` for a type
/// that shares its name with a value the module exports (`type Color = …; const Color = …`);
/// `extend:<type>` for an `extend` block (which shares its type's name), `extend:<type>~2` for
/// the second block on that type in the module, and so on.
fn key(items: &[DocItem], i: usize) -> String {
    let item = &items[i];
    if shares_name_with_value(items, item) {
        return format!("type:{}", item.name);
    }
    if item.kind != Kind::Extension {
        return item.name.clone();
    }
    let n = items[..i]
        .iter()
        .filter(|it| it.kind == Kind::Extension && it.name == item.name)
        .count();
    match n {
        0 => format!("extend:{}", item.name),
        n => format!("extend:{}~{}", item.name, n + 1),
    }
}

/// Whether `item` is a type and `items` (its module's exports) has a value of the same name.
fn shares_name_with_value(items: &[DocItem], item: &DocItem) -> bool {
    is_type(item.kind)
        && items
            .iter()
            .any(|it| it.name == item.name && !is_type(it.kind) && it.kind != Kind::Extension)
}

/// A member's part of an `id`: its name; `<name>:set` for a setter (a getter has the plain
/// name), `static:<name>` for a static member whose name an instance member of `parent` has too.
fn member_key(parent: &DocItem, member: &DocItem) -> String {
    if kind_and_modifiers(member).0 == "setter" {
        return format!("{}:set", member.name);
    }
    let is_static = |m: &DocItem| m.signature.starts_with("static ");
    let clash = parent
        .members
        .iter()
        .any(|m| m.name == member.name && is_static(m) != is_static(member));
    match is_static(member) && clash {
        true => format!("static:{}", member.name),
        false => member.name.clone(),
    }
}

/// `{id, module, kind, name, member}`: where a link leads (to `member` of the `parent` that
/// `t` names, when given).
fn target(t: &Target, member: Option<(&DocItem, &DocItem)>, ids: Ids) -> Json {
    let mut id = match is_type(t.kind) && ids.clash.contains(&(t.module.as_str(), t.name.as_str()))
    {
        true => format!("{}#type:{}", specifier(&t.module), t.name),
        false => format!("{}#{}", specifier(&t.module), t.name),
    };
    if let Some((parent, m)) = member {
        id = format!("{id}.{}", member_key(parent, m));
    }
    Json::Obj(vec![
        ("id", Json::Str(id)),
        ("module", Json::str(&t.module)),
        ("kind", Json::str(t.kind.label())),
        ("name", Json::str(&t.name)),
        ("member", Json::opt(member.map(|(_, m)| m.name.as_str()))),
    ])
}

/// The linked type names of `signature`: `{start, end, role, target}`, with offsets in UTF-16
/// code units (JavaScript string indices) and the place of the name in the declaration
/// ([`role`]).
fn spans(
    signature: &str,
    field: bool,
    generics: &[&str],
    scope: &HashMap<String, Target>,
    self_href: &str,
    ids: Ids,
) -> Json {
    let mut out = vec![];
    let (mut at, mut byte) = (0u64, 0usize);
    for part in signature_parts(signature, generics, scope, self_href) {
        let len = part.text.encode_utf16().count() as u64;
        if let Some(t) = part.target {
            // The overload's line the name is on.
            let line_start = signature[..byte].rfind('\n').map_or(0, |i| i + 1);
            let line_end = signature[byte..]
                .find('\n')
                .map_or(signature.len(), |i| byte + i);
            let line = &signature[line_start..line_end];
            out.push(Json::Obj(vec![
                ("start", Json::Num(at)),
                ("end", Json::Num(at + len)),
                ("role", Json::str(role(line, byte - line_start, field))),
                ("target", target(t, None, ids)),
            ]));
        }
        at += len;
        byte += part.text.len();
    }
    Json::Arr(out)
}

/// Where in the declaration `line` (one line of a canonical signature) the type name at byte
/// `at` is: `param`, `return`, `throws`, `extends`, `implements`, `typeArg` (a bound or default
/// of the declaration's own type parameters), `field` (the type of a field: `field`), `type` (of
/// a constant or type alias, or the type an `extend` block extends). A type argument (`Promise<Point>`) has the role of the
/// type it is an argument of.
fn role(line: &str, at: usize, field: bool) -> &'static str {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        Head,
        Params,
        AfterParams,
        Return,
        Throws,
        Extends,
        Implements,
        Type,
    }
    let bytes = line.as_bytes();
    let mut section = Section::Head;
    // Nesting of `()`, `[]`, `{}` (`depth`) and `<>` (`angles`) within the current section.
    let (mut depth, mut angles) = (0i32, 0i32);
    let mut extend_block = false;
    let mut i = 0;
    while i < at && i < bytes.len() {
        let c = bytes[i];
        let top = depth == 0 && angles == 0;
        let word_at = |w: &str| {
            line[i..].starts_with(w)
                && (i == 0 || !is_word_byte(bytes[i - 1]))
                && !line[i + w.len()..].starts_with(|c: char| is_word_byte(c as u8))
        };
        match c {
            b'"' | b'\'' | b'`' => {
                i += 1;
                while i < bytes.len() && bytes[i] != c {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'=' if bytes.get(i + 1) == Some(&b'>') => i += 1, // `=>` in a function type
            b'(' if top && section == Section::Head => {
                section = Section::Params;
                depth = 1;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' if depth == 1 && section == Section::Params => {
                section = Section::AfterParams;
                depth = 0;
            }
            b')' | b']' | b'}' => depth -= 1,
            b'<' => angles += 1,
            b'>' => angles -= 1,
            b':' if top && section == Section::AfterParams => section = Section::Return,
            b':' | b'=' if top && section == Section::Head => section = Section::Type,
            _ if top && word_at("throws") => section = Section::Throws,
            _ if top && word_at("extends") && section == Section::Head => {
                section = Section::Extends
            }
            _ if top && word_at("implements") => section = Section::Implements,
            _ if top && section == Section::Head && word_at("extend") => {
                extend_block = true;
                i += "extend".len() - 1;
            }
            // The type an `extend` block extends, after its type parameters.
            _ if top && section == Section::Head && extend_block && is_word_byte(c) => {
                section = Section::Type
            }
            _ => {}
        }
        i += 1;
    }
    match section {
        Section::Head if angles > 0 => "typeArg",
        Section::Head | Section::Type if field => "field",
        Section::Head | Section::Type => "type",
        Section::Params => "param",
        Section::AfterParams | Section::Return => "return",
        Section::Throws => "throws",
        Section::Extends => "extends",
        Section::Implements => "implements",
    }
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

fn tags(c: &DocComment) -> Json {
    Json::Obj(vec![
        (
            "params",
            Json::Arr(
                c.params
                    .iter()
                    .map(|(name, doc)| {
                        Json::Obj(vec![("name", Json::str(name)), ("doc", Json::text(doc))])
                    })
                    .collect(),
            ),
        ),
        ("returns", Json::opt(c.returns.as_deref())),
        ("throws", Json::strs(&c.throws)),
        ("examples", Json::strs(&c.examples)),
        ("deprecated", Json::opt(c.deprecated.as_deref())),
        ("see", Json::strs(&c.see)),
    ])
}

/// Modifier words a signature may start with.
const MODIFIERS: &[&str] = &[
    "static",
    "async",
    "get",
    "set",
    "readonly",
    "abstract",
    "protected",
    "public",
    "mut",
];

/// The item's kind in the JSON (`getter` / `setter` for accessors, otherwise
/// [`Kind::label`]) and its modifiers: the ones its signatures start with (all overloads'),
/// and `deprecated`.
fn kind_and_modifiers(item: &DocItem) -> (&'static str, Vec<Json>) {
    let mut words: Vec<&str> = vec![];
    for line in item.signature.lines() {
        for w in line.split_whitespace() {
            if !MODIFIERS.contains(&w) {
                break;
            }
            if !words.contains(&w) {
                words.push(w);
            }
        }
    }
    let kind = match item.kind {
        Kind::Method if words.contains(&"get") => "getter",
        Kind::Method if words.contains(&"set") => "setter",
        k => k.label(),
    };
    let mut modifiers: Vec<Json> = MODIFIERS
        .iter()
        .filter(|m| !matches!(**m, "get" | "set") && words.contains(m))
        .map(|m| Json::str(m))
        .collect();
    if item.deprecated.is_some() {
        modifiers.push(Json::str("deprecated"));
    }
    (kind, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_json() {
        let v = Json::Obj(vec![
            ("a", Json::Arr(vec![Json::Num(1), Json::Null])),
            ("b", Json::str("q\"\\\n\u{1}é")),
            ("c", Json::Arr(vec![])),
            ("d", Json::Obj(vec![])),
            ("e", Json::Bool(true)),
        ]);
        assert_eq!(
            v.pretty(),
            "{\n  \"a\": [\n    1,\n    null\n  ],\n  \"b\": \"q\\\"\\\\\\n\\u0001é\",\n  \
             \"c\": [],\n  \"d\": {},\n  \"e\": true\n}\n"
        );
    }

    #[test]
    fn specifiers() {
        assert_eq!(specifier("std/fs"), "velt:fs");
        assert_eq!(specifier("std/prelude/core"), "velt:prelude/core");
        assert_eq!(specifier("pkg/util"), "pkg/util");
    }
}
