//! Notes for a name that isn't found (`Ctx::not_found`): where a type Velt knows the name of lives.

/// The note for `name`, not found (`msg` is the error: a type or a value).
pub(crate) fn note(msg: &str, name: &str) -> Option<String> {
    if crate::hir::SimdTy::named(name).is_some() {
        return Some(format!(
            "import it: `import {{ {name} }} from \"velt/simd\";`"
        ));
    }
    let is_type = msg.starts_with("cannot find type");
    (is_type && crate::lib_dom_names::is_lib_dom_type(name)).then(|| {
        format!("`{name}` is a type of TypeScript's `lib.dom` that std doesn't declare yet (std/lang/dom, scripts/dom-subset/names.txt)")
    })
}
