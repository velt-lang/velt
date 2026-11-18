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
fn attributes_are_not_the_parameters_or_the_result() {
    let split = |ty: &str| split_function_type(ty);
    let none = Vec::<String>::new;
    assert_eq!(
        split("long (const char *, int)"),
        ("long".into(), false, none())
    );
    assert_eq!(
        split("int (void) __attribute__((cdecl))"),
        ("int".into(), false, vec!["cdecl".into()])
    );
    assert_eq!(
        split("char *(const char *) __attribute__((__cdecl__))"),
        ("char *".into(), false, vec!["cdecl".into()])
    );
    assert_eq!(
        split("int (const char *, ...) __attribute__((cdecl)) __attribute__((noreturn))"),
        ("int".into(), true, vec!["cdecl".into(), "noreturn".into()])
    );
    assert_eq!(
        split("int (int) __attribute__((ms_abi))"),
        ("int".into(), false, vec!["ms_abi".into()])
    );
    assert_eq!(
        split("__attribute__((preserve_most)) int (int)"),
        ("int".into(), false, vec!["preserve_most".into()])
    );
    assert_eq!(
        split("int (int) __attribute__((pcs(\"aapcs\")))"),
        ("int".into(), false, vec!["pcs(\"aapcs\")".into()])
    );
    assert_eq!(
        split("int (const char *, ...)"),
        ("int".into(), true, none())
    );
    assert_eq!(
        split("void (void (*)(int, ...))"),
        ("void".into(), false, none())
    );
    assert_eq!(split("int"), ("int".into(), false, none()));
}

/// glibc's `__SOCKADDR_ARG` with `_GNU_SOURCE`, as clang prints it: a typedef of a union
/// without a tag, which the C ABI passes as its first member.
#[test]
fn a_transparent_union_is_its_first_member() {
    let field = |ty: &str| serde_json::json!({ "kind": "FieldDecl", "type": { "qualType": ty } });
    let json = serde_json::json!({ "inner": [
        { "kind": "RecordDecl", "id": "0x1", "tagUsed": "union", "inner": [
            { "kind": "TransparentUnionAttr" },
            field("struct sockaddr *restrict"),
            field("struct sockaddr_in *restrict")
        ] },
        { "kind": "TypedefDecl", "name": "__SOCKADDR_ARG", "type": { "qualType": "union __SOCKADDR_ARG" },
          "inner": [{ "kind": "RecordType", "decl": { "id": "0x1", "kind": "RecordDecl", "name": "" } }] },
        { "kind": "RecordDecl", "id": "0x2", "tagUsed": "union", "inner": [field("int"), field("long")] },
        { "kind": "TypedefDecl", "name": "plain", "type": { "qualType": "union plain" },
          "inner": [{ "kind": "RecordType", "decl": { "id": "0x2", "kind": "RecordDecl", "name": "" } }] }
    ]});
    let h = Headers::from_json(&json);
    assert_eq!(
        h.resolve("__SOCKADDR_ARG", 64),
        CType::Pointer {
            pointee: Box::new(CType::Named("sockaddr".into())),
            is_const: false,
            pointee_names: vec!["sockaddr".into()],
        }
    );
    assert_eq!(h.resolve("plain", 64), CType::Named("plain".into()));
}

/// What clang prints for the enums of a header.
#[test]
fn an_enum_is_as_wide_as_its_type_says() {
    let constant =
        |ty: &str| serde_json::json!({ "kind": "EnumConstantDecl", "type": { "qualType": ty } });
    let json = serde_json::json!({ "inner": [
        { "kind": "EnumDecl", "name": "plain", "inner": [constant("int")] },
        { "kind": "EnumDecl", "name": "wide", "inner": [constant("int"), constant("unsigned long")] },
        { "kind": "EnumDecl", "name": "fixed", "fixedUnderlyingType": { "qualType": "long long" }, "inner": [constant("long long")] },
        { "kind": "EnumDecl", "name": "bytes", "fixedUnderlyingType": { "qualType": "char" } },
        { "kind": "EnumDecl", "name": "bytes" },
        { "kind": "EnumDecl", "name": "tiny", "inner": [{ "kind": "PackedAttr" }, constant("int")] },
        { "kind": "EnumDecl", "name": "byte", "inner": [{ "kind": "ModeAttr" }, constant("int")] },
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
    assert_eq!(
        h.resolve("enum only_named", 64),
        CType::Unknown("enum only_named".into())
    );
    assert_eq!(
        h.resolve("enum tiny", 64),
        CType::Unknown("packed enum tiny".into())
    );
    assert_eq!(
        h.resolve("enum byte", 64),
        CType::Unknown("mode enum byte".into())
    );
}

#[test]
fn const_is_about_what_the_pointer_points_at() {
    let json = serde_json::json!({ "inner": [
        { "kind": "TypedefDecl", "name": "fixed_text", "type": { "qualType": "char *const" } },
        { "kind": "TypedefDecl", "name": "text", "type": { "qualType": "const char *" } }
    ]});
    let h = Headers::from_json(&json);
    let is_const = |written: &str| match h.resolve(written, 64) {
        CType::Pointer { is_const, .. } => is_const,
        other => panic!("{other:?}"),
    };
    for yes in [
        "const char *",
        "char const *",
        "char *const *",
        "const char *const *",
        "fixed_text *",
    ] {
        assert!(is_const(yes), "{yes}");
    }
    for no in [
        "char *",
        "const char **",
        "text *",
        "char *const",
        "const char *restrict *",
    ] {
        assert!(!is_const(no), "{no}");
    }
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
        CType::Unknown("enum mode".into())
    );
    assert_eq!(h.resolve("__int128", 64), CType::Unknown("__int128".into()));
}

/// Structs are found by tag, through a typedef of a tagged struct, and through a typedef of one
/// without a tag; a definition wins over a declaration, and what C can't share is marked.
#[test]
fn structs_are_found_by_tag_and_typedef() {
    let field = |name: &str, ty: &str| serde_json::json!({ "kind": "FieldDecl", "name": name, "type": { "qualType": ty } });
    let json = serde_json::json!({ "inner": [
        { "kind": "RecordDecl", "name": "point", "tagUsed": "struct", "loc": { "file": "/p/geo.h", "line": 2 } },
        { "kind": "RecordDecl", "name": "point", "tagUsed": "struct", "completeDefinition": true,
          "loc": { "line": 3 }, "inner": [field("x", "double"), field("y", "double")] },
        { "kind": "TypedefDecl", "name": "point_t", "loc": { "line": 4 }, "type": { "qualType": "struct point" } },
        { "kind": "TypedefDecl", "name": "pt", "loc": { "line": 5 }, "type": { "qualType": "point_t" } },
        { "kind": "RecordDecl", "id": "0x9", "tagUsed": "struct", "completeDefinition": true,
          "loc": { "line": 6 }, "inner": [
            { "kind": "FieldDecl", "name": "flags", "isBitfield": true, "type": { "qualType": "unsigned int" } }
          ] },
        { "kind": "TypedefDecl", "name": "box_t", "loc": { "line": 6 }, "type": { "qualType": "struct box_t" },
          "inner": [ { "kind": "ElaboratedType", "decl": { "id": "0x9" } } ] },
        { "kind": "RecordDecl", "name": "either", "tagUsed": "union", "completeDefinition": true,
          "loc": { "line": 7 }, "inner": [field("a", "int")] },
        { "kind": "RecordDecl", "name": "tight", "tagUsed": "struct", "completeDefinition": true,
          "loc": { "line": 8 }, "inner": [field("a", "char"), { "kind": "PackedAttr" }] }
    ]});
    let h = Headers::from_json(&json);
    let (point, spelling) = h.record("point").unwrap();
    assert_eq!(spelling, "struct point");
    assert!(point.defined && !point.union && !point.packed);
    assert_eq!(point.at, "geo.h:3");
    assert_eq!(
        point.fields[1],
        CField {
            name: "y".into(),
            ty: "double".into(),
            bitfield: false
        }
    );
    for name in ["point_t", "pt"] {
        let (r, spelling) = h.record(name).unwrap();
        assert_eq!((r, spelling.as_str()), (point, name));
    }
    let (b, spelling) = h.record("box_t").unwrap();
    assert_eq!(spelling, "box_t");
    assert!(b.fields[0].bitfield);
    assert_eq!(h.record("either").unwrap().1, "union either");
    assert!(h.record("either").unwrap().0.union);
    assert!(h.record("tight").unwrap().0.packed);
    assert!(h.record("nothing").is_none());
}

/// A struct defined inside another one has its own line, not the outer struct's.
#[test]
fn a_nested_struct_has_its_own_line() {
    let json = serde_json::json!({ "inner": [
        { "kind": "RecordDecl", "name": "outer", "tagUsed": "struct", "completeDefinition": true,
          "loc": { "file": "/p/geo.h", "line": 2 },
          "range": { "begin": { "line": 2 }, "end": { "line": 6 } },
          "inner": [
            { "kind": "FieldDecl", "name": "a", "loc": { "line": 3 }, "type": { "qualType": "int" } },
            { "kind": "RecordDecl", "name": "inner", "tagUsed": "struct", "completeDefinition": true,
              "loc": { "line": 4 }, "range": { "begin": { "line": 4 }, "end": { "line": 4 } },
              "inner": [ { "kind": "FieldDecl", "name": "b", "loc": { "line": 4 }, "type": { "qualType": "int" } } ] },
            { "kind": "FieldDecl", "name": "in", "loc": { "line": 5 }, "type": { "qualType": "struct inner" } }
          ] }
    ]});
    let h = Headers::from_json(&json);
    assert_eq!(h.record("outer").unwrap().0.at, "geo.h:2");
    assert_eq!(h.record("inner").unwrap().0.at, "geo.h:4");
}
