//! std's modules as the app's source rewrites see them, by units (#2943 M2): std's own rewrites
//! ran in std's stage, so the app's stage gives the pipeline (`crate::rewrites`) a stub of each
//! std module holding only what the rewrites read of other modules:
//!
//! - imports and re-exports (`crate::generic_arrows`, `crate::overloads`, `crate::global_this`
//!   follow them to what an app module imports);
//! - type aliases (aliases of function types and alias chains, `generic_arrows`; function types
//!   an interface extends, `crate::call_sigs`);
//! - interfaces (what an augmentation targets, `crate::interface_merge`);
//! - overloaded functions and every class, without bodies (`overloads`: exported overload sets,
//!   and each class's overloaded methods and base, which a subclass's walk follows by name);
//! - module-level string constants (`global_this`'s keys).
//!
//! A stub's own rewrites change nothing that matters: what they make of std is dropped, and std's
//! facts come from std's stage. On disk, a unit keeps its stubs (they need no other std AST).

use velt_syntax::ast;

use crate::units::codec::AstNode;
use crate::SourceModule;

/// The stub of std module `m`.
pub(crate) fn stub(m: &SourceModule) -> SourceModule {
    let items = (m.ast.items.iter()).filter_map(stub_item).collect();
    SourceModule {
        path: m.path.clone(),
        is_std: m.is_std,
        is_dependency: m.is_dependency,
        file: m.file,
        ast: ast::Module {
            items,
            span: m.ast.span,
            jsx_import_source: m.ast.jsx_import_source.clone(),
            fn_names: Default::default(),
        },
        imports: m.imports.clone(),
        jsx_runtime: m.jsx_runtime.clone(),
    }
}

fn stub_item(i: &ast::Item) -> Option<ast::Item> {
    use ast::ItemKind as I;
    let kind = match &i.kind {
        I::Import(_) | I::TypeAlias(_) => i.kind.clone(),
        I::Interface(d) => I::Interface(d.kept().into_owned()),
        I::Function(f) if !f.overloads.is_empty() => I::Function(f.kept().into_owned()),
        I::Class(d) => I::Class(d.kept().into_owned()),
        I::Struct(d) => I::Struct(d.kept().into_owned()),
        I::Var(v) if crate::global_this::keys::const_key(v).is_some() => i.kind.clone(),
        _ => return None,
    };
    Some(ast::Item {
        kind,
        exported: i.exported,
        span: i.span,
    })
}

/// The modules the app's rewrites run over: each std module as its [`stub`], the others as
/// they are.
pub(crate) fn stubbed(modules: &[SourceModule]) -> Vec<SourceModule> {
    (modules.iter())
        .map(|m| match m.is_std {
            true => stub(m),
            false => copy(m),
        })
        .collect()
}

/// A copy of module `m`.
pub(crate) fn copy(m: &SourceModule) -> SourceModule {
    SourceModule {
        path: m.path.clone(),
        is_std: m.is_std,
        is_dependency: m.is_dependency,
        file: m.file,
        ast: m.ast.clone(),
        imports: m.imports.clone(),
        jsx_runtime: m.jsx_runtime.clone(),
    }
}

/// The facts of the rewrites, put together from std's own run (`std`, over std's modules alone)
/// and the app's run (`app`, over [`stubbed`] modules): std's spans from std's run, the app's
/// from the app's (what the app's run found in std's stubs is dropped). std has no namespaces,
/// global properties, brands, promoted constants or nested closures, so those are the app's.
pub(crate) struct Merged {
    pub lifted: Option<crate::generic_arrows::Lifted>,
    pub overloaded: Option<crate::overloads::Rewritten>,
    pub alias_blocked: crate::recursive_aliases::Blocked,
}

pub(crate) fn merge(
    std: &crate::rewrites::Rewritten,
    app: &crate::rewrites::Rewritten,
    modules: &[SourceModule],
) -> Merged {
    use rustc_hash::FxHashSet;
    use velt_common::Span;
    let std_files: FxHashSet<velt_common::FileId> = (modules.iter())
        .filter(|m| m.is_std)
        .map(|m| m.file)
        .collect();
    let own = |s: &Span| !std_files.contains(&s.file);
    let sets = |a: Option<&FxHashSet<Span>>, b: Option<&FxHashSet<Span>>| -> FxHashSet<Span> {
        let mut out: FxHashSet<Span> = a.into_iter().flatten().copied().collect();
        out.extend(b.into_iter().flatten().copied().filter(own));
        out
    };
    let lifted = (std.lifted.is_some() || app.lifted.is_some()).then(|| {
        let (s, a) = (std.lifted.as_ref(), app.lifted.as_ref());
        crate::generic_arrows::Lifted {
            modules: vec![],
            local_fns: sets(s.map(|l| &l.local_fns), a.map(|l| &l.local_fns)),
            all_fns: sets(s.map(|l| &l.all_fns), a.map(|l| &l.all_fns)),
            unsafe_fns: sets(s.map(|l| &l.unsafe_fns), a.map(|l| &l.unsafe_fns)),
        }
    });
    let overloaded = (std.overloaded.is_some() || app.overloaded.is_some()).then(|| {
        let (s, a) = (std.overloaded.as_ref(), app.overloaded.as_ref());
        let mut out = crate::overloads::Rewritten {
            modules: vec![],
            sig_spans: s.map(|o| o.sig_spans.clone()).unwrap_or_default(),
            methods: s.map(|o| o.methods.clone()).unwrap_or_default(),
            adapted: s.map(|o| o.adapted.clone()).unwrap_or_default(),
        };
        if let Some(a) = a {
            out.sig_spans.extend(
                a.sig_spans
                    .iter()
                    .filter(|(k, _)| own(k))
                    .map(|(k, v)| (*k, *v)),
            );
            out.methods.extend(a.methods.iter().cloned());
            out.adapted
                .extend(a.adapted.iter().filter(|x| own(&x.imp.0)).cloned());
        }
        out
    });
    let mut alias_blocked = std.alias_blocked.clone();
    alias_blocked.extend(
        (app.alias_blocked.iter())
            .filter(|(k, _)| own(k))
            .map(|(k, v)| (*k, v.clone())),
    );
    Merged {
        lifted,
        overloaded,
        alias_blocked,
    }
}
