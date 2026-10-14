//! `import.meta` (`ast::IMPORT_META`): the parser adds a module constant with empty string fields
//! to a module that mentions it, and the loader, which knows the module's file, fills them in as
//! Node does: `url` is the file's `file:` URL, `filename` its absolute path and `dirname` its
//! directory. They name the source file, fixed while compiling.

use std::path::Path;

use velt_syntax::ast;

/// Fills in the fields of `module`'s `import.meta` constant, if it has one, for the file `path`.
pub(super) fn fill(module: &mut ast::Module, path: &Path) {
    let Some(init) = module.items.iter_mut().rev().find_map(|i| match &mut i.kind {
        ast::ItemKind::Var(ast::VarDecl {
            pattern:
                ast::Pattern {
                    kind: ast::PatternKind::Ident(name),
                    ..
                },
            init: Some(init),
            ..
        }) if name.name == ast::IMPORT_META => Some(init),
        _ => None,
    }) else {
        return;
    };
    let file = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = file.parent().unwrap_or(&file).to_path_buf();
    let values = [
        file_url(&file),
        file.display().to_string(),
        dir.display().to_string(),
    ];
    let ast::ExprKind::Object(props) = &mut init.kind else {
        return;
    };
    for (p, v) in props.iter_mut().zip(values) {
        if let ast::ObjectProp::KeyValue(_, e) = p {
            e.kind = ast::ExprKind::Lit(ast::Lit::Str(v));
        }
    }
}

/// The `file:` URL of the absolute path `file`, as Node's `url.pathToFileURL` gives it:
/// `/`-separated (a Windows drive path gets a leading `/`), with the bytes a URL path cannot
/// hold percent-encoded.
pub(super) fn file_url(file: &Path) -> String {
    let text = file.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut path = match cfg!(windows) {
        true => text.replace('\\', "/"),
        false => text.to_string(),
    };
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    let mut out = String::from("file://");
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric() || b"/-._!$&'()*+,;=:@".contains(&b);
        match keep {
            true => out.push(b as char),
            false => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::file_url;
    use std::path::Path;

    #[test]
    fn file_urls() {
        assert_eq!(file_url(Path::new("/a/b c/d.ts")), "file:///a/b%20c/d.ts");
        assert_eq!(file_url(Path::new("/x/%#?é.ts")), "file:///x/%25%23%3F%C3%A9.ts");
        if cfg!(windows) {
            assert_eq!(
                file_url(Path::new(r"C:\Dev\m.ts")),
                "file:///C:/Dev/m.ts"
            );
        }
    }
}
