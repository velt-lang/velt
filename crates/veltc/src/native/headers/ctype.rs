//! What clang says about C headers (`-ast-dump=json`): the functions they declare and the C
//! types of their parameters and results, with typedefs resolved for one target.

use std::collections::HashMap;

use serde_json::Value;

/// A function of the headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CFunction {
    /// The result's C type, as written (`sqlite3_int64`, `const char *`).
    pub ret: String,
    /// Each parameter's C type, as written.
    pub params: Vec<String>,
    pub variadic: bool,
    /// Where it is declared: the header's file name and line.
    pub at: String,
}

/// The functions and typedefs of a translation unit.
#[derive(Default, Debug)]
pub struct Headers {
    pub functions: HashMap<String, CFunction>,
    /// Typedef name → the type it names, as written.
    typedefs: HashMap<String, String>,
    /// Enum tag (a typedef's name, for an enum without a tag) → what its width comes from.
    enums: HashMap<String, EnumDef>,
    /// Enums without a tag, by clang's id, until a typedef names them.
    unnamed_enums: HashMap<String, EnumDef>,
}

/// What an enum's width comes from, as written.
#[derive(Clone, Debug, Default)]
struct EnumDef {
    /// `enum e : long long`.
    fixed: Option<String>,
    /// Its constants' types: `int`, or a wider type for a constant `int` doesn't hold.
    constants: Vec<String>,
    /// `__attribute__((packed))`: as narrow as its values, which only clang works out.
    packed: bool,
}

impl EnumDef {
    /// `None` for a declaration that only names the enum (`enum e;`).
    fn of(node: &Value) -> Option<EnumDef> {
        let inner = node["inner"].as_array();
        let fixed = node["fixedUnderlyingType"]["qualType"]
            .as_str()
            .map(str::to_string);
        if inner.is_none() && fixed.is_none() {
            return None;
        }
        let of_kind = |kind: &'static str| {
            inner
                .into_iter()
                .flatten()
                .filter(move |n| n["kind"] == kind)
        };
        Some(EnumDef {
            fixed,
            constants: of_kind("EnumConstantDecl")
                .filter_map(|c| c["type"]["qualType"].as_str().map(str::to_string))
                .collect(),
            packed: of_kind("PackedAttr").next().is_some(),
        })
    }
}

impl Headers {
    /// Read clang's JSON dump of a translation unit.
    pub fn from_json(json: &Value) -> Headers {
        let mut h = Headers::default();
        let mut place = Place::default();
        if let Some(inner) = json["inner"].as_array() {
            for node in inner {
                h.top_level(node, &mut place);
            }
        }
        h
    }

    fn top_level(&mut self, node: &Value, place: &mut Place) {
        let line = node_line(node, place);
        let at = format!("{}:{line}", short(&place.file));
        match node["kind"].as_str() {
            Some("FunctionDecl") if node["isImplicit"] != true => {
                let name = node["name"].as_str().unwrap_or_default().to_string();
                let ty = node["type"]["qualType"].as_str().unwrap_or_default();
                let params: Vec<String> = node["inner"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["kind"] == "ParmVarDecl")
                    .map(|p| {
                        p["type"]["qualType"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect();
                let (ret, variadic) = split_function_type(ty);
                // The first declaration wins: a later one repeats it (a definition, a redeclaration).
                self.functions.entry(name).or_insert(CFunction {
                    ret,
                    params,
                    variadic,
                    at,
                });
            }
            Some("TypedefDecl") => {
                if let (Some(name), Some(ty)) =
                    (node["name"].as_str(), node["type"]["qualType"].as_str())
                {
                    self.typedefs.insert(name.to_string(), ty.to_string());
                    // `typedef enum { A } mode;`: clang prints the type as `enum mode`.
                    let id = node["inner"][0]["decl"]["id"].as_str().unwrap_or_default();
                    if let Some(def) = self.unnamed_enums.get(id) {
                        self.enums.insert(name.to_string(), def.clone());
                    }
                }
            }
            _ => self.nested_enums(node),
        }
        visit_places(&node["inner"], place);
    }

    /// Enums `node` defines, inside structs too: their tags are the file's in C.
    fn nested_enums(&mut self, node: &Value) {
        match node["kind"].as_str() {
            Some("EnumDecl") => {
                let Some(def) = EnumDef::of(node) else { return };
                match node["name"].as_str() {
                    Some(tag) => {
                        self.enums.insert(tag.to_string(), def);
                    }
                    None => {
                        let id = node["id"].as_str().unwrap_or_default().to_string();
                        self.unnamed_enums.insert(id, def);
                    }
                }
            }
            Some("RecordDecl") => {
                for inner in node["inner"].as_array().into_iter().flatten() {
                    self.nested_enums(inner);
                }
            }
            _ => {}
        }
    }

    /// The integer type of `enum tag`. An enum without a fixed type is as wide as its widest
    /// constant, and its sign is clang's choice, which depends on its values: either matches.
    fn enum_type(&self, tag: &str, long_bits: u32, names: &[String]) -> CType {
        let def = self.enums.get(tag);
        let resolve = |written: &str| self.resolve_in(written, long_bits, &mut names.to_vec());
        match def.and_then(|d| d.fixed.as_deref()) {
            Some(fixed) => match resolve(fixed) {
                CType::Int {
                    bits,
                    signed,
                    plain_char,
                    ..
                } => CType::Enum {
                    bits,
                    signed: (!plain_char).then_some(signed),
                },
                _ => CType::Unknown(format!("enum {tag}")),
            },
            None if def.is_some_and(|d| d.packed) => CType::Unknown(format!("packed enum {tag}")),
            None => {
                let constants = def.map(|d| d.constants.as_slice()).unwrap_or_default();
                let bits = constants
                    .iter()
                    .map(|c| match resolve(c) {
                        CType::Int { bits, .. } => bits,
                        _ => 32,
                    })
                    .max()
                    .unwrap_or(32);
                CType::Enum { bits, signed: None }
            }
        }
    }

    /// The C type `written` (`size_t`, `const char *`) for a target whose `long` has `long_bits`.
    pub fn resolve(&self, written: &str, long_bits: u32) -> CType {
        self.resolve_in(written, long_bits, &mut Vec::new())
    }

    fn resolve_in(&self, written: &str, long_bits: u32, names: &mut Vec<String>) -> CType {
        let compact: String = written.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("(*") || compact.contains("(^") {
            return CType::FunctionPointer;
        }
        let t = clean(written);
        // `char *const` is a constant pointer: the `const` after the `*` is the pointer's own.
        let t = t.strip_suffix(" const").unwrap_or(&t).trim().to_string();
        if let Some(inner) = t.strip_suffix('*') {
            let inner = inner.trim();
            let is_const = inner.split_whitespace().any(|w| w == "const");
            return CType::Pointer {
                pointee: Box::new(self.resolve_in(inner, long_bits, &mut Vec::new())),
                is_const,
                pointee_names: typedef_names(inner),
            };
        }
        // `int [4]`, `char []` as a parameter: a pointer.
        if let Some((elem, _)) = t.split_once('[') {
            return self.resolve_in(&format!("{} *", elem.trim()), long_bits, names);
        }
        let words: Vec<&str> = t
            .split_whitespace()
            .filter(|w| *w != "const" && *w != "volatile")
            .collect();
        let base = words.join(" ");
        if let Some(scalar) = builtin(&words, long_bits) {
            return match scalar {
                CType::Int { bits, signed, .. } => CType::Int {
                    bits,
                    signed,
                    names: names.clone(),
                    plain_char: words == ["char"],
                },
                other => other,
            };
        }
        if let Some(tag) = base
            .strip_prefix("struct ")
            .or_else(|| base.strip_prefix("union "))
        {
            return CType::Named(tag.to_string());
        }
        if let Some(tag) = base.strip_prefix("enum ") {
            return self.enum_type(tag, long_bits, names);
        }
        match self.typedefs.get(&base) {
            // A typedef of itself (`typedef struct x x`) names the struct.
            Some(under) if names.len() < 32 && !names.contains(&base) => {
                names.push(base.clone());
                match self.resolve_in(under, long_bits, names) {
                    CType::Named(_) => CType::Named(base),
                    other => other,
                }
            }
            // clang's own pointer-sized types, which no header defines (64 bits on every
            // target with C headers).
            _ if matches!(base.as_str(), "__size_t" | "__SIZE_TYPE__")
                || matches!(
                    base.as_str(),
                    "__ptrdiff_t" | "__signed_size_t" | "__PTRDIFF_TYPE__"
                ) =>
            {
                let mut names = names.clone();
                names.push(base.clone());
                CType::Int {
                    bits: 64,
                    signed: !matches!(base.as_str(), "__size_t" | "__SIZE_TYPE__"),
                    names,
                    plain_char: false,
                }
            }
            _ => CType::Unknown(base),
        }
    }
}

/// A C type, typedefs resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CType {
    Void,
    /// An integer type; `names` are the typedefs it was named through (`size_t`, `__size_t`),
    /// `plain_char` that it is `char`, whose signedness is the target's.
    Int {
        bits: u32,
        signed: bool,
        names: Vec<String>,
        plain_char: bool,
    },
    Float,
    Double,
    Bool,
    Pointer {
        pointee: Box<CType>,
        is_const: bool,
        /// The pointee's written name and the typedefs it goes through (`sqlite3` of
        /// `sqlite3 *`), to match a `declare c type`.
        pointee_names: Vec<String>,
    },
    /// A struct or union, by tag or typedef name (behind a pointer: a `declare c type`).
    Named(String),
    /// An enum: an integer of `bits`; `signed`, when its type fixes the sign.
    Enum {
        bits: u32,
        signed: Option<bool>,
    },
    FunctionPointer,
    /// A type the check doesn't know (`__int128`, a vector type).
    Unknown(String),
}

/// The names a pointee is written with: its last word (`sqlite3` of `const sqlite3`, `FILE`).
fn typedef_names(written: &str) -> Vec<String> {
    written
        .split_whitespace()
        .rfind(|w| !matches!(*w, "const" | "volatile" | "struct" | "union"))
        .map(|w| vec![w.to_string()])
        .unwrap_or_default()
}

/// `written` without the qualifiers and attributes that don't change what crosses.
fn clean(written: &str) -> String {
    let spaced = written.replace('*', " * ");
    let words: Vec<&str> = spaced
        .split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "restrict"
                    | "__restrict"
                    | "__restrict__"
                    | "_Nonnull"
                    | "_Nullable"
                    | "_Null_unspecified"
                    | "__unaligned"
            )
        })
        .collect();
    words.join(" ")
}

fn builtin(words: &[&str], long_bits: u32) -> Option<CType> {
    let unsigned = words.contains(&"unsigned");
    let signed = !unsigned;
    let rest: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| *w != "unsigned" && *w != "signed")
        .collect();
    let bits = match rest.as_slice() {
        ["void"] => return Some(CType::Void),
        ["_Bool"] | ["bool"] => return Some(CType::Bool),
        ["float"] => return Some(CType::Float),
        ["double"] => return Some(CType::Double),
        ["char"] => 8,
        ["short"] | ["short", "int"] => 16,
        [] | ["int"] => {
            if words.is_empty() {
                return None;
            }
            32
        }
        ["long"] | ["long", "int"] => long_bits,
        ["long", "long"] | ["long", "long", "int"] => 64,
        _ => return None,
    };
    Some(CType::Int {
        bits,
        signed,
        names: vec![],
        plain_char: false,
    })
}

/// A function type's result and whether it is variadic: `long (const char *, char **, int)`
/// → (`long`, false). Attributes after the parameters are skipped: `int (void)
/// __attribute__((cdecl))`, which is how clang prints a calling convention.
fn split_function_type(ty: &str) -> (String, bool) {
    let mut end = ty.trim_end().len();
    while let Some(open) = opening_paren(&ty[..end]) {
        let before = ty[..open].trim_end();
        match before.strip_suffix("__attribute__") {
            Some(rest) => end = rest.trim_end().len(),
            None => {
                let params = ty[open + 1..end - 1].trim_end();
                return (before.to_string(), params.ends_with("..."));
            }
        }
    }
    (ty.to_string(), false)
}

/// Where the parenthesis that the `)` closing `text` opens is; `None` when `text` doesn't end
/// with one.
fn opening_paren(text: &str) -> Option<usize> {
    if !text.ends_with(')') {
        return None;
    }
    let mut depth = 0i32;
    for (i, b) in text.bytes().enumerate().rev() {
        match b {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The file and line clang printed last: it leaves either out of a location when it is the
/// same, in print order (a node's `loc`, then its `range`, then its children).
#[derive(Default)]
struct Place {
    file: String,
    line: u64,
}

impl Place {
    fn see(&mut self, loc: &Value) {
        for l in [&loc["spellingLoc"], &loc["expansionLoc"], loc] {
            if let Some(f) = l["file"].as_str() {
                self.file = f.to_string();
            }
            if let Some(n) = l["line"].as_u64() {
                self.line = n;
            }
        }
    }
}

/// The line of `node`'s location (where a macro made it, for one a macro made), following
/// `place` through the node's own locations.
fn node_line(node: &Value, place: &mut Place) -> u64 {
    place.see(&node["loc"]);
    let line = place.line;
    for edge in ["begin", "end"] {
        place.see(&node["range"][edge]);
    }
    line
}

/// Follow `place` through every location below `inner`, in print order.
fn visit_places(inner: &Value, place: &mut Place) {
    for node in inner.as_array().into_iter().flatten() {
        node_line(node, place);
        visit_places(&node["inner"], place);
    }
}

/// A header's path as messages show it: its file name.
fn short(file: &str) -> &str {
    file.rsplit(['/', '\\']).next().unwrap_or(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of clang's dump: the file is printed once, then left out.
    fn headers() -> Headers {
        let json = serde_json::json!({ "inner": [
            { "kind": "TypedefDecl", "name": "__size_t", "loc": { "file": "/z/stddef.h", "line": 3 },
              "type": { "qualType": "unsigned long" } },
            { "kind": "TypedefDecl", "name": "size_t", "loc": { "line": 4 }, "type": { "qualType": "__size_t" } },
            { "kind": "TypedefDecl", "name": "sqlite3", "loc": { "file": "/p/native/sqlite3.h", "line": 10 },
              "type": { "qualType": "struct sqlite3" } },
            { "kind": "TypedefDecl", "name": "sqlite3_int64", "loc": { "line": 11 }, "type": { "qualType": "long long" } },
            { "kind": "FunctionDecl", "name": "sqlite3_bind_text", "loc": { "line": 4581 },
              "type": { "qualType": "int (sqlite3 *, int, const char *, int, void (*)(void *))" },
              "inner": [
                { "kind": "ParmVarDecl", "loc": { "line": 4581 }, "type": { "qualType": "sqlite3 *" } },
                { "kind": "ParmVarDecl", "loc": { "line": 4581 }, "type": { "qualType": "int" } },
                { "kind": "ParmVarDecl", "loc": { "line": 4581 }, "type": { "qualType": "const char *" } },
                { "kind": "ParmVarDecl", "loc": { "line": 4581 }, "type": { "qualType": "int" } },
                { "kind": "ParmVarDecl", "loc": { "line": 4581 }, "type": { "qualType": "void (*)(void *)" } }
              ] },
            { "kind": "FunctionDecl", "name": "printf", "loc": { "file": "/z/stdio.h", "line": 7 },
              "type": { "qualType": "int (const char *restrict, ...)" },
              "inner": [ { "kind": "ParmVarDecl", "type": { "qualType": "const char *restrict" } } ] }
        ]});
        Headers::from_json(&json)
    }

    #[test]
    fn functions_and_their_places_are_read() {
        let h = headers();
        let f = &h.functions["sqlite3_bind_text"];
        assert_eq!(f.ret, "int");
        assert_eq!(f.params.len(), 5);
        assert!(!f.variadic);
        assert_eq!(f.at, "sqlite3.h:4581");
        let p = &h.functions["printf"];
        assert!(p.variadic);
        assert_eq!(p.at, "stdio.h:7");
    }

    #[test]
    fn attributes_after_the_parameters_are_not_the_parameters() {
        let split = |ty: &str| split_function_type(ty);
        assert_eq!(split("long (const char *, int)"), ("long".into(), false));
        assert_eq!(
            split("int (void) __attribute__((cdecl))"),
            ("int".into(), false)
        );
        assert_eq!(
            split("char *(const char *) __attribute__((__cdecl__))"),
            ("char *".into(), false)
        );
        assert_eq!(
            split("int (const char *, ...) __attribute__((cdecl)) __attribute__((noreturn))"),
            ("int".into(), true)
        );
        assert_eq!(split("int (const char *, ...)"), ("int".into(), true));
        assert_eq!(split("void (void (*)(int, ...))"), ("void".into(), false));
        assert_eq!(split("int"), ("int".into(), false));
    }

    /// What clang prints for the enums of a header.
    #[test]
    fn an_enum_is_as_wide_as_its_type_says() {
        let constant = |ty: &str| serde_json::json!({ "kind": "EnumConstantDecl", "type": { "qualType": ty } });
        let json = serde_json::json!({ "inner": [
            { "kind": "EnumDecl", "name": "plain", "inner": [constant("int")] },
            { "kind": "EnumDecl", "name": "wide", "inner": [constant("int"), constant("unsigned long")] },
            { "kind": "EnumDecl", "name": "fixed", "fixedUnderlyingType": { "qualType": "long long" }, "inner": [constant("long long")] },
            { "kind": "EnumDecl", "name": "bytes", "fixedUnderlyingType": { "qualType": "char" } },
            { "kind": "EnumDecl", "name": "bytes" },
            { "kind": "EnumDecl", "name": "tiny", "inner": [{ "kind": "PackedAttr" }, constant("int")] },
            { "kind": "EnumDecl", "id": "0x1", "inner": [constant("long long")] },
            { "kind": "TypedefDecl", "name": "mode", "type": { "qualType": "enum mode" },
              "inner": [{ "kind": "EnumType", "decl": { "id": "0x1", "kind": "EnumDecl", "name": "" } }] },
            { "kind": "RecordDecl", "name": "s", "inner": [
                { "kind": "EnumDecl", "name": "inside", "inner": [constant("long")] }
            ] }
        ]});
        let h = Headers::from_json(&json);
        let e = |bits, signed| CType::Enum { bits, signed };
        assert_eq!(h.resolve("enum plain", 64), e(32, None));
        assert_eq!(h.resolve("enum wide", 64), e(64, None));
        assert_eq!(h.resolve("enum fixed", 64), e(64, Some(true)));
        assert_eq!(h.resolve("enum bytes", 64), e(8, None));
        assert_eq!(h.resolve("mode", 64), e(64, None));
        assert_eq!(h.resolve("enum inside", 64), e(64, None));
        assert_eq!(h.resolve("enum inside", 32), e(32, None));
        assert_eq!(h.resolve("enum only_named", 64), e(32, None));
        assert_eq!(
            h.resolve("enum tiny", 64),
            CType::Unknown("packed enum tiny".into())
        );
    }

    #[test]
    fn types_resolve_per_target() {
        let h = headers();
        let size = |bits| CType::Int {
            bits,
            signed: false,
            names: vec!["size_t".into(), "__size_t".into()],
            plain_char: false,
        };
        assert_eq!(h.resolve("size_t", 64), size(64));
        assert_eq!(h.resolve("size_t", 32), size(32));
        assert!(matches!(
            h.resolve("long", 32),
            CType::Int {
                bits: 32,
                signed: true,
                ..
            }
        ));
        assert!(matches!(
            h.resolve("sqlite3_int64", 32),
            CType::Int { bits: 64, .. }
        ));
        assert!(matches!(
            h.resolve("char", 64),
            CType::Int {
                plain_char: true,
                ..
            }
        ));
        match h.resolve("const char *restrict", 64) {
            CType::Pointer {
                pointee, is_const, ..
            } => {
                assert!(is_const);
                assert!(matches!(*pointee, CType::Int { bits: 8, .. }));
            }
            other => panic!("{other:?}"),
        }
        match h.resolve("sqlite3 **", 64) {
            CType::Pointer {
                pointee, is_const, ..
            } => {
                assert!(!is_const);
                match *pointee {
                    CType::Pointer {
                        pointee_names,
                        pointee,
                        ..
                    } => {
                        assert_eq!(pointee_names, ["sqlite3"]);
                        assert_eq!(*pointee, CType::Named("sqlite3".into()));
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(h.resolve("void (*)(void *)", 64), CType::FunctionPointer);
        assert!(matches!(
            h.resolve("char *const", 64),
            CType::Pointer {
                is_const: false,
                ..
            }
        ));
        assert_eq!(h.resolve("_Bool", 64), CType::Bool);
        assert_eq!(
            h.resolve("enum mode", 64),
            CType::Enum {
                bits: 32,
                signed: None
            }
        );
        assert_eq!(h.resolve("__int128", 64), CType::Unknown("__int128".into()));
    }
}
