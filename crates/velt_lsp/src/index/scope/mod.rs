//! Walks the document's AST towards a cursor offset, keeping a stack of local scopes, and reports
//! the name under the cursor ([`Reference`]) and the locals visible there.
//!
//! Only the path to the cursor is walked: statements before it contribute their bindings without
//! being entered, statements after it are skipped. Bindings follow the language's scoping: `let`/
//! `const` are visible after their statement, parameters in the whole body, `for` bindings in the
//! loop, pattern bindings in their match arm or catch block. Items and types are walked here,
//! statements and patterns in [`stmt`], expressions in [`expr`], JSX elements in [`jsx`].

mod expr;
mod jsx;
mod stmt;

pub(crate) use jsx::is_component;

use velt_common::Span;
use velt_syntax::ast;

use super::{type_name, Decl, DeclKind, LocalBinding};
use crate::analysis::Analysis;
use crate::signature;

/// What is at the cursor.
#[derive(Default)]
pub struct CursorInfo<'a> {
    /// The name under the cursor, if any.
    pub reference: Option<Reference<'a>>,
    /// Locals in scope at the cursor (outer scopes first; later entries shadow earlier ones).
    pub visible: Vec<LocalBinding>,
    /// The class/struct/interface whose body contains the cursor (the type of `this`).
    pub owner: Option<&'a ast::Item>,
}

/// A name occurrence under the cursor.
#[derive(Clone, Debug)]
pub enum Reference<'a> {
    /// A value or type name; `local` if it resolved to a local binding.
    Name {
        /// The identifier.
        ident: &'a ast::Ident,
        /// The local it refers to (or declares).
        local: Option<LocalBinding>,
    },
    /// `this.prop`.
    ThisMember(&'a ast::Ident),
    /// `object.prop` with any other object.
    Member {
        /// The receiver expression.
        object: &'a ast::Expr,
        /// The member name.
        prop: &'a ast::Ident,
    },
    /// Segment `index` of a dotted path (`Shape.Circle` in a pattern or type).
    Path {
        /// All segments.
        path: &'a [ast::Ident],
        /// The segment under the cursor.
        index: usize,
    },
    /// A name in `import { name } from "..."`.
    Import {
        /// The import declaration.
        import: &'a ast::Import,
        /// The imported name.
        name: &'a ast::ImportName,
    },
    /// The name identifier of an item or member declaration.
    Declared(Decl<'a>),
}

impl Reference<'_> {
    /// Span of the identifier under the cursor.
    pub fn span(&self) -> Span {
        match self {
            Reference::Name { ident, .. } => ident.span,
            Reference::ThisMember(prop) | Reference::Member { prop, .. } => prop.span,
            Reference::Path { path, index } => path[*index].span,
            Reference::Import { name, .. } => name.alias.as_ref().unwrap_or(&name.name).span,
            Reference::Declared(d) => d.name_span,
        }
    }
}

/// Walk the document of `analysis` to byte `offset`.
pub fn at_offset(analysis: &Analysis, offset: u32) -> CursorInfo<'_> {
    let mut w = Walker {
        analysis,
        module: analysis.root,
        offset,
        scopes: vec![vec![]],
        owners: vec![],
        info: CursorInfo::default(),
        snapped: false,
    };
    for item in &analysis.module().ast.items {
        if w.contains(item.span) {
            w.item(item);
        }
    }
    w.snapshot();
    w.info
}

struct Walker<'a> {
    analysis: &'a Analysis,
    module: usize,
    offset: u32,
    scopes: Vec<Vec<LocalBinding>>,
    owners: Vec<&'a ast::Item>,
    info: CursorInfo<'a>,
    snapped: bool,
}

impl<'a> Walker<'a> {
    fn contains(&self, span: Span) -> bool {
        span.lo <= self.offset && self.offset <= span.hi
    }

    /// Record the scopes as they are now (first call wins: that is the cursor's position).
    fn snapshot(&mut self) {
        if !self.snapped {
            self.snapped = true;
            self.info.visible = self.scopes.iter().flatten().cloned().collect();
            self.info.owner = self.owners.last().copied();
        }
    }

    fn hit(&mut self, reference: Reference<'a>) {
        if self.info.reference.is_none() {
            self.snapshot();
            self.info.reference = Some(reference);
        }
    }

    fn lookup(&self, name: &str) -> Option<LocalBinding> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|b| b.name == name)
            .cloned()
    }

    fn bind(&mut self, ident: &'a ast::Ident, detail: String, type_name: Option<String>) {
        let binding = LocalBinding {
            name: ident.name.clone(),
            span: ident.span,
            detail,
            type_name,
        };
        if self.contains(ident.span) {
            self.hit(Reference::Name {
                ident,
                local: Some(binding.clone()),
            });
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.push(binding);
        }
    }

    fn declared(&mut self, ident: &'a ast::Ident, kind: DeclKind<'a>) {
        if self.contains(ident.span) {
            self.hit(Reference::Declared(Decl {
                module: self.module,
                name: ident.name.clone(),
                name_span: ident.span,
                kind,
            }));
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(vec![]);
        f(self);
        self.scopes.pop();
    }

    // ── items ──

    fn item(&mut self, item: &'a ast::Item) {
        for ident in super::item_names(item) {
            self.declared(ident, DeclKind::Item(item));
        }
        match &item.kind {
            ast::ItemKind::Import(import) => self.import(import),
            ast::ItemKind::Function(f) => self.function(&f.sig, Some(&f.body)),
            ast::ItemKind::ExternFn(sig) => self.function(sig, None),
            ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
                self.owners.push(item);
                self.type_decl(t);
                self.owners.pop();
            }
            ast::ItemKind::Interface(i) => {
                self.owners.push(item);
                self.interface(i);
                self.owners.pop();
            }
            ast::ItemKind::Enum(e) => self.enum_decl(e),
            ast::ItemKind::TypeAlias(a) => self.ty(&a.ty),
            ast::ItemKind::Var(v) => {
                self.opt_ty(v.ty.as_ref());
                self.opt_expr(v.init.as_ref());
            }
            ast::ItemKind::Extend(ext) => {
                self.ty(&ext.target);
                for m in &ext.methods {
                    self.function(&m.decl.sig, Some(&m.decl.body));
                }
            }
        }
    }

    fn import(&mut self, import: &'a ast::Import) {
        for name in &import.names {
            let alias = name.alias.as_ref().is_some_and(|a| self.contains(a.span));
            if alias || self.contains(name.name.span) {
                self.hit(Reference::Import { import, name });
            }
        }
    }

    pub(super) fn function(&mut self, sig: &'a ast::FnSig, body: Option<&'a ast::Block>) {
        self.scoped(|w| {
            for p in &sig.params {
                w.ty(&p.ty);
                w.opt_expr(p.default.as_ref());
                let detail = signature::param(w.analysis, p);
                w.bind(&p.name, detail, type_name(&p.ty).map(String::from));
            }
            w.opt_ty(sig.ret.as_ref());
            w.opt_ty(sig.throws.as_ref());
            if let Some(body) = body.filter(|b| w.contains(b.span)) {
                w.stmts(&body.stmts);
                w.snapshot();
            }
        });
    }

    fn type_decl(&mut self, t: &'a ast::TypeDecl) {
        let owner = t.name.name.clone();
        self.opt_ty(t.extends.as_ref());
        t.implements.iter().for_each(|ty| self.ty(ty));
        self.fields(&t.fields, &owner);
        if let Some(ctor) = &t.constructor {
            self.declared(
                &ctor.sig.name,
                DeclKind::Constructor(&ctor.sig, owner.clone()),
            );
            self.function(&ctor.sig, Some(&ctor.body));
        }
        for m in &t.methods {
            let kind = DeclKind::Method(&m.decl.sig, owner.clone(), m.is_static);
            self.declared(&m.decl.sig.name, kind);
            self.function(&m.decl.sig, Some(&m.decl.body));
        }
    }

    fn interface(&mut self, i: &'a ast::InterfaceDecl) {
        let owner = i.name.name.clone();
        i.extends.iter().for_each(|ty| self.ty(ty));
        self.fields(&i.fields, &owner);
        for m in &i.methods {
            let kind = DeclKind::Method(&m.sig, owner.clone(), false);
            self.declared(&m.sig.name, kind);
            self.function(&m.sig, m.body.as_ref());
        }
    }

    fn fields(&mut self, fields: &'a [ast::Field], owner: &str) {
        for f in fields {
            if !self.contains(f.span) {
                continue;
            }
            self.declared(&f.name, DeclKind::Field(f, owner.into()));
            self.ty(&f.ty);
            self.opt_expr(f.default.as_ref());
        }
    }

    fn enum_decl(&mut self, e: &'a ast::EnumDecl) {
        for v in &e.variants {
            if !self.contains(v.span) {
                continue;
            }
            self.declared(&v.name, DeclKind::Variant(v, e.name.name.clone()));
            self.opt_expr(v.discriminant.as_ref());
        }
    }

    // ── types ──

    fn opt_ty(&mut self, ty: Option<&'a ast::TypeExpr>) {
        if let Some(ty) = ty {
            self.ty(ty);
        }
    }

    fn ty(&mut self, ty: &'a ast::TypeExpr) {
        if !self.contains(ty.span) {
            return;
        }
        match &ty.kind {
            ast::TypeExprKind::Named { path, args } => {
                self.path(path);
                args.iter().for_each(|a| self.ty(a));
            }
            ast::TypeExprKind::Array(elem) => self.ty(elem),
            ast::TypeExprKind::Indexed { object, key } => {
                self.ty(object);
                self.ty(key);
            }
            ast::TypeExprKind::Tuple(tys)
            | ast::TypeExprKind::Union(tys)
            | ast::TypeExprKind::Intersection(tys) => tys.iter().for_each(|t| self.ty(t)),
            ast::TypeExprKind::Function {
                params,
                ret,
                throws,
            } => {
                params.iter().for_each(|t| self.ty(t));
                self.ty(ret);
                throws.iter().for_each(|t| self.ty(t));
            }
            ast::TypeExprKind::Object(fields) => fields.iter().for_each(|f| self.ty(&f.ty)),
            ast::TypeExprKind::Literal(_) | ast::TypeExprKind::Null | ast::TypeExprKind::Void => {}
        }
    }

    /// A dotted name (`User`, `Shape.Circle`): single segments are plain names.
    fn path(&mut self, path: &'a [ast::Ident]) {
        let Some(index) = path.iter().position(|seg| self.contains(seg.span)) else {
            return;
        };
        if path.len() == 1 {
            let local = self.lookup(&path[0].name);
            self.hit(Reference::Name {
                ident: &path[0],
                local,
            });
        } else {
            self.hit(Reference::Path { path, index });
        }
    }
}
