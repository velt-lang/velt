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
    /// The attributes of its type, by name (`ms_abi`, `noreturn`): calling conventions.
    pub attributes: Vec<String>,
    /// Where it is declared: the header's file name and line.
    pub at: String,
}

/// A variable of the headers (`extern char **environ;`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CVariable {
    /// Its C type, as written (`char **`, `const int`).
    pub ty: String,
    /// `_Thread_local` / `__thread`: each thread has its own, which a link symbol doesn't name.
    pub thread_local: bool,
    /// `static`: the header defines it for each file that includes it, under no link symbol.
    pub internal: bool,
    /// Where it is declared: the header's file name and line.
    pub at: String,
}

/// A struct or union of the headers, as clang defines it (`RecordDecl`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CRecord {
    /// Which definition it is: `struct tag` (`union tag`), or clang's id for one without a tag.
    pub key: String,
    /// `union`, not `struct`.
    pub union: bool,
    /// Defined, not only declared (`struct s;`): only a definition has fields.
    pub defined: bool,
    /// `__attribute__((packed))`.
    pub packed: bool,
    pub fields: Vec<CField>,
    /// Where it is declared (defined, for a defined one): the header's file name and line.
    pub at: String,
}

/// A field of a [`CRecord`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CField {
    /// Empty for an anonymous struct or union member.
    pub name: String,
    /// Its C type, as written (`const char *`, `int [4]`, `struct point`).
    pub ty: String,
    pub bitfield: bool,
}

/// The functions, variables and typedefs of a translation unit.
#[derive(Default, Debug)]
pub struct Headers {
    pub functions: HashMap<String, CFunction>,
    pub variables: HashMap<String, CVariable>,
    /// Structs and unions with a tag, by tag: the definition if there is one.
    records: HashMap<String, CRecord>,
    /// Structs and unions without a tag, by clang's id, until a typedef names them.
    unnamed_records: HashMap<String, CRecord>,
    /// Typedef name → the struct or union without a tag it names.
    typedef_records: HashMap<String, CRecord>,
    /// The `declare c struct`s checked with these headers, by name: a message about a pointer
    /// to a struct names the declared one for its definition (`structs`).
    pub declared: Vec<String>,
    /// Typedef name → the type it names, as written.
    typedefs: HashMap<String, String>,
    /// Enum tag (a typedef's name, for an enum without a tag) → what its width comes from.
    enums: HashMap<String, EnumDef>,
    /// Enums without a tag, by clang's id, until a typedef names them.
    unnamed_enums: HashMap<String, EnumDef>,
    /// Transparent unions without a tag, by clang's id → their first member's type, as written,
    /// until a typedef names them.
    unnamed_transparent: HashMap<String, String>,
}

/// What an enum's width comes from, as written.
#[derive(Clone, Debug, Default)]
struct EnumDef {
    /// `enum e : long long`.
    fixed: Option<String>,
    /// Its constants' types: `int`, or a wider type for a constant `int` doesn't hold.
    constants: Vec<String>,
    /// `__attribute__((packed))` or `__attribute__((mode(byte)))`, which make it narrower than
    /// its constants say: what only clang works out. The attribute, as messages show it.
    narrowed: Option<&'static str>,
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
            narrowed: [("PackedAttr", "packed"), ("ModeAttr", "mode")]
                .into_iter()
                .find(|(kind, _)| of_kind(kind).next().is_some())
                .map(|(_, attribute)| attribute),
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
                let (ret, variadic, attributes) = split_function_type(ty);
                // The first declaration wins: a later one repeats it (a definition, a redeclaration).
                self.functions.entry(name).or_insert(CFunction {
                    ret,
                    params,
                    variadic,
                    attributes,
                    at,
                });
            }
            Some("VarDecl") if node["isImplicit"] != true => {
                let name = node["name"].as_str().unwrap_or_default().to_string();
                let ty = node["type"]["qualType"].as_str().unwrap_or_default();
                // The first declaration wins, as for a function.
                self.variables.entry(name).or_insert(CVariable {
                    ty: ty.to_string(),
                    thread_local: node.get("tls").is_some_and(|t| !t.is_null()),
                    internal: node["storageClass"] == "static",
                    at,
                });
            }
            Some("TypedefDecl") => {
                if let (Some(name), Some(ty)) =
                    (node["name"].as_str(), node["type"]["qualType"].as_str())
                {
                    // `typedef enum { A } mode;`: clang prints the type as `enum mode`.
                    let id = node["inner"][0]["decl"]["id"].as_str().unwrap_or_default();
                    if let Some(def) = self.unnamed_enums.get(id) {
                        self.enums.insert(name.to_string(), def.clone());
                    }
                    if let Some(r) = self.unnamed_records.get(id) {
                        self.typedef_records.insert(name.to_string(), r.clone());
                    }
                    // A transparent union is passed as its first member (glibc's
                    // `__SOCKADDR_ARG`, `struct sockaddr *` to the C ABI): the typedef names that.
                    let ty = self.unnamed_transparent.get(id).map_or(ty, String::as_str);
                    self.typedefs.insert(name.to_string(), ty.to_string());
                }
            }
            Some("RecordDecl") if node["name"].as_str().is_none() => {
                if let Some(first) = transparent_union_member(node) {
                    let id = node["id"].as_str().unwrap_or_default().to_string();
                    self.unnamed_transparent.insert(id, first);
                }
                let id = node["id"].as_str().unwrap_or_default().to_string();
                self.unnamed_records.insert(id, record_of(node, at.clone()));
                self.nested_enums(node);
                self.nested_records(node, place);
            }
            Some("RecordDecl") => {
                self.add_record(node, at.clone());
                self.nested_enums(node);
                self.nested_records(node, place);
            }
            _ => self.nested_enums(node),
        }
        visit_places(&node["inner"], place);
    }

    /// Records the struct or union `node` (with a tag), declared at `at`: a definition replaces
    /// a declaration, and the first definition wins.
    fn add_record(&mut self, node: &Value, at: String) {
        let Some(tag) = node["name"].as_str() else {
            return;
        };
        let r = record_of(node, at);
        match self.records.get(tag) {
            Some(old) if old.defined || !r.defined => {}
            _ => {
                self.records.insert(tag.to_string(), r);
            }
        }
    }

    /// Structs and unions with a tag defined inside `node`'s fields: their tags are the file's in
    /// C, as enums' are. `place` is where clang's printing is after `node`'s own locations: a
    /// copy follows it through the fields, so each nested record gets its own line.
    fn nested_records(&mut self, node: &Value, place: &Place) {
        let mut place = place.clone();
        self.nested_records_in(node, &mut place);
    }

    fn nested_records_in(&mut self, node: &Value, place: &mut Place) {
        for inner in node["inner"].as_array().into_iter().flatten() {
            let line = node_line(inner, place);
            if inner["kind"] == "RecordDecl" {
                self.add_record(inner, format!("{}:{line}", short(&place.file)));
                self.nested_records_in(inner, place);
            } else {
                visit_places(&inner["inner"], place);
            }
        }
    }

    /// The struct or union C type `name` (of a `declare c struct`) is: its tag (`struct name`),
    /// or a typedef naming one; and how C spells the type (`struct name`, `name`).
    pub fn record(&self, name: &str) -> Option<(&CRecord, String)> {
        if let Some(r) = self.records.get(name) {
            return Some((
                r,
                format!("{} {name}", if r.union { "union" } else { "struct" }),
            ));
        }
        if let Some(r) = self.typedef_records.get(name) {
            return Some((r, name.to_string()));
        }
        // `typedef struct tag name;`, through typedefs of typedefs.
        let mut written = self.typedefs.get(name)?.clone();
        for _ in 0..32 {
            let base = clean(&written);
            let words: Vec<&str> = base
                .split_whitespace()
                .filter(|w| *w != "const" && *w != "volatile")
                .collect();
            if let ["struct" | "union", tag] = words.as_slice() {
                return self.records.get(*tag).map(|r| (r, name.to_string()));
            }
            if let Some(r) = self.typedef_records.get(&words.join(" ")) {
                return Some((r, name.to_string()));
            }
            written = self.typedefs.get(&words.join(" "))?.clone();
        }
        None
    }

    /// Do `a` and `b` (a `declare c struct`'s name, a C type's) name the same struct or union
    /// definition, each by its tag or a typedef (`point` and `point_t` of `typedef struct point
    /// { … } point_t;`)?
    pub fn same_record(&self, a: &str, b: &str) -> bool {
        match (self.record(a), self.record(b)) {
            (Some((x, _)), Some((y, _))) => x.key == y.key,
            _ => false,
        }
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
    /// One whose definition the dump doesn't show (only declared, or defined in a function's
    /// parameters, whose scope clang doesn't print) is unknown.
    fn enum_type(&self, tag: &str, long_bits: u32, names: &[String]) -> CType {
        let Some(def) = self.enums.get(tag) else {
            return CType::Unknown(format!("enum {tag}"));
        };
        let resolve = |written: &str| self.resolve_in(written, long_bits, &mut names.to_vec());
        match (def.fixed.as_deref(), def.narrowed) {
            (Some(fixed), _) => match resolve(fixed) {
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
            (None, Some(attribute)) => CType::Unknown(format!("{attribute} enum {tag}")),
            (None, None) => {
                let bits = (def.constants.iter())
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

    /// Whether C can't write through a pointer to `written`: `const` qualifies `written` itself,
    /// not what it points at. `const char` and `char *const` are; `const char *` isn't, so C
    /// writes the pointer a `const char **` points at.
    fn is_const(&self, written: &str, depth: u32) -> bool {
        let own = written.rsplit('*').next().unwrap_or(written);
        if own.split_whitespace().any(|w| w == "const") {
            return true;
        }
        if written.contains('*') || depth >= 32 {
            return false;
        }
        let base: Vec<&str> = own
            .split_whitespace()
            .filter(|w| *w != "volatile")
            .collect();
        match self.typedefs.get(&base.join(" ")) {
            Some(under) if under.trim() != base.join(" ") => self.is_const(under, depth + 1),
            _ => false,
        }
    }

    /// Is an object of C type `written` itself `const` (`const int`, `char *const`, a typedef
    /// of one), not only what it points to?
    pub fn object_is_const(&self, written: &str) -> bool {
        self.is_const(written, 0)
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
            let is_const = self.is_const(inner, 0);
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

/// The struct or union `node` (a `RecordDecl`), declared at `at`.
fn record_of(node: &Value, at: String) -> CRecord {
    let inner = node["inner"].as_array();
    let fields = (inner.into_iter().flatten())
        .filter(|n| n["kind"] == "FieldDecl")
        .map(|f| CField {
            name: f["name"].as_str().unwrap_or_default().to_string(),
            ty: f["type"]["qualType"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            bitfield: f["isBitfield"] == true,
        })
        .collect();
    let union = node["tagUsed"] == "union";
    let key = match node["name"].as_str() {
        Some(tag) => format!("{} {tag}", if union { "union" } else { "struct" }),
        None => node["id"].as_str().unwrap_or_default().to_string(),
    };
    CRecord {
        key,
        union,
        defined: node["completeDefinition"] == true,
        packed: (inner.into_iter().flatten()).any(|n| n["kind"] == "PackedAttr"),
        fields,
        at,
    }
}

/// The type of `node`'s first member, as written, when `node` is a transparent union
/// (`__attribute__((transparent_union))`), which the C ABI passes as that member.
fn transparent_union_member(node: &Value) -> Option<String> {
    let inner = node["inner"].as_array()?;
    if node["tagUsed"] != "union" || !inner.iter().any(|n| n["kind"] == "TransparentUnionAttr") {
        return None;
    }
    let first = inner.iter().find(|n| n["kind"] == "FieldDecl")?;
    first["type"]["qualType"].as_str().map(str::to_string)
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

/// A function type's result, whether it is variadic and its attributes: `long (const char *,
/// int)` → (`long`, false, []). clang prints a calling convention as an attribute after the
/// parameters (`int (void) __attribute__((ms_abi))`) or, for some, before the result
/// (`__attribute__((preserve_most)) int (int)`); each comes back by its name (`ms_abi`, with
/// `__cdecl__` as `cdecl`).
fn split_function_type(ty: &str) -> (String, bool, Vec<String>) {
    let mut attributes = Vec::new();
    let mut end = ty.trim_end().len();
    while let Some(open) = opening_paren(&ty[..end]) {
        let before = ty[..open].trim_end();
        match before.strip_suffix("__attribute__") {
            Some(rest) => {
                attributes.push(attribute_name(&ty[open..end]));
                end = rest.trim_end().len();
            }
            None => {
                let params = ty[open + 1..end - 1].trim_end();
                attributes.reverse();
                let mut ret = before;
                while let Some(rest) = ret.strip_prefix("__attribute__") {
                    let Some(close) = closing_paren(rest) else {
                        break;
                    };
                    attributes.push(attribute_name(&rest[..=close]));
                    ret = rest[close + 1..].trim_start();
                }
                return (ret.to_string(), params.ends_with("..."), attributes);
            }
        }
    }
    (ty.to_string(), false, attributes)
}

/// The name of the attribute `((__cdecl__))`, `((pcs("aapcs")))`: `cdecl`, `pcs("aapcs")`.
fn attribute_name(parens: &str) -> String {
    let parens = parens.trim();
    let inner = (parens.strip_prefix("((").and_then(|p| p.strip_suffix("))"))).unwrap_or(parens);
    let (name, args) = inner.split_at(inner.find('(').unwrap_or(inner.len()));
    let name = name.trim();
    let name = (name.strip_prefix("__").and_then(|n| n.strip_suffix("__"))).unwrap_or(name);
    format!("{name}{args}")
}

/// Where the `)` closing the `(` that `text` starts with is; `None` when it doesn't start with
/// one, or it isn't closed.
fn closing_paren(text: &str) -> Option<usize> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    for (i, b) in text.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
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
#[derive(Clone, Default)]
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
#[path = "ctype_tests.rs"]
mod tests;
