//! The build script's scan for the runtime's exported functions (`abi_symbols.rs`, the table
//! the `velt dev` JIT host registers): comments must not change which functions it finds, as
//! the change planner (crates/xtask) treats a comment edit as no code change.

// Only the scan is tested; the rest runs as the build script.
#[allow(dead_code)]
#[path = "../build.rs"]
mod build;

use build::exported_functions;

#[test]
fn comments_between_the_attribute_and_the_function_are_skipped() {
    let src = r#"
#[no_mangle]
/// Docs.
// SAFETY: the caller passes a valid pointer.
/* A block
   comment. */
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn velt_rt_a(p: *const u8) {}

#[no_mangle] // trailing
pub extern "C" fn velt_rt_b() {}
"#;
    assert_eq!(exported_functions(src), ["velt_rt_a", "velt_rt_b"]);
}

#[test]
fn commented_out_functions_are_not_exported() {
    let src = r#"
// #[no_mangle]
// pub extern "C" fn velt_rt_line() {}
/*
#[no_mangle]
pub extern "C" fn velt_rt_block() {}
*/
#[no_mangle]
pub extern "C" fn velt_rt_kept() {}
"#;
    assert_eq!(exported_functions(src), ["velt_rt_kept"]);
}

#[test]
fn comment_markers_in_literals_open_no_comment() {
    let src = r#"
const A: &str = "accept: */* /* not a comment";
const B: char = '/';
#[no_mangle]
pub extern "C" fn velt_rt_after_strings() {}
const C: &str = r"*/ // still a string";
#[no_mangle]
pub extern "C" fn velt_rt_after_raw() {}
"#;
    assert_eq!(
        exported_functions(src),
        ["velt_rt_after_strings", "velt_rt_after_raw"]
    );
}

#[test]
fn other_items_after_the_attribute_are_not_functions() {
    let src = "#[no_mangle]\npub static VELT_RT_X: u8 = 0;\nfn helper() {}\n#[no_mangle]\npub extern \"C\" fn main() {}\n";
    assert!(exported_functions(src).is_empty());
}
