//! Which namespaces see the members other modules declare in `declare global { namespace N }`
//! ([`super::flatten_items`]): a `declare global` block's, not a `declare module "m"` block's,
//! which TypeScript keeps as that module's. The parser refuses a namespace in `declare module`
//! today, so the second block is made from the first.

use rustc_hash::FxHashMap;
use velt_common::{FileId, Span};
use velt_syntax::ast;

use super::{flatten_items, Members, Private};

/// The flattened items of `declare global { namespace JSX { interface H extends
/// IntrinsicAttributes {} } }`, as a `declare module "m"` block with `module`, when another
/// module's global `JSX` declares `IntrinsicAttributes`.
fn flattened(module: bool) -> Vec<ast::Item> {
    let src = "declare global { namespace JSX { interface H extends IntrinsicAttributes {} } }\nexport {};\n";
    let (mut m, diags) = velt_syntax::parse_file(FileId(0), src);
    assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");
    for item in &mut m.items {
        if let ast::ItemKind::Augment(a) = &mut item.kind {
            if module {
                a.module = Some(("m".to_string(), Span::DUMMY));
            }
        }
    }
    let mut members = Members::default();
    let name = "IntrinsicAttributes".to_string();
    members.types.insert(name.clone(), name);
    let mut globals = FxHashMap::default();
    globals.insert(vec!["JSX".to_string()], members);
    flatten_items(m.items, &globals, &mut Private::default())
}

/// The path `H`'s `extends` names after flattening.
fn parent_of_h(items: &[ast::Item]) -> Vec<String> {
    let h = items.iter().find_map(|i| match &i.kind {
        ast::ItemKind::Interface(d) if d.name.name == "JSX.H" => Some(d),
        _ => None,
    });
    let h = h.expect("the flattened `JSX.H`");
    match &h.extends[0].kind {
        ast::TypeExprKind::Named { path, .. } => path.iter().map(|p| p.name.clone()).collect(),
        _ => vec![],
    }
}

#[test]
fn a_declare_global_namespace_sees_other_modules_members() {
    assert_eq!(
        parent_of_h(&flattened(false)),
        ["JSX", "IntrinsicAttributes"]
    );
}

#[test]
fn a_declare_module_namespace_does_not() {
    assert_eq!(parent_of_h(&flattened(true)), ["IntrinsicAttributes"]);
}
