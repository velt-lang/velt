//! Items: imports, functions, `declare function`, type aliases and variables, plus the blank-line
//! policy between top-level items. Nominal type declarations are in `decls`.

use velt_syntax::ast::{Import, Item, ItemKind, TypeAlias, TypeExprKind, VarDecl};

use super::lists::delimited;
use super::Printer;
use crate::doc::{cat, group, if_break, indent, join, line, text, Doc};
use crate::source::{slice, string_literal};

/// Items of these kinds may sit on consecutive lines (a run of imports, constants, ...);
/// every other pair of top-level items is separated by a blank line.
fn compact_kind(item: &Item) -> Option<u8> {
    match item.kind {
        ItemKind::Import(_) => Some(0),
        ItemKind::Var(_) => Some(1),
        ItemKind::TypeAlias(_) => Some(2),
        ItemKind::ExternFn(_) => Some(3),
        _ => None,
    }
}

/// May `item` sit on the line next to another top-level entry?
pub(super) fn is_compact(item: &Item) -> bool {
    compact_kind(item).is_some()
}

/// Is a blank line required between two consecutive top-level items?
pub(super) fn blank_between(prev: &Item, cur: &Item) -> bool {
    match (compact_kind(prev), compact_kind(cur)) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

impl<'a> Printer<'a> {
    pub(super) fn item(&mut self, item: &Item) -> Doc {
        let export = if item.exported { "export " } else { "" };
        let body = match &item.kind {
            ItemKind::Import(import) => self.import(import, item.exported),
            ItemKind::Function(f) => self.fn_decl(f, "function "),
            ItemKind::Struct(decl) => self.type_decl("struct ", decl),
            ItemKind::Class(decl) => self.type_decl("class ", decl),
            ItemKind::Interface(decl) => self.interface(decl),
            ItemKind::Enum(decl) => self.enum_decl(decl),
            ItemKind::TypeAlias(alias) => self.type_alias(alias),
            ItemKind::Var(var) => cat![self.var_decl(var), ";"],
            ItemKind::Extend(decl) => self.extend(decl),
            ItemKind::ExternFn(sig) => {
                let asyncness = if sig.is_async { "async " } else { "" };
                let sig = self.fn_sig(sig, "function ", sig.span.hi);
                cat!["declare ", asyncness, sig, ";"]
            }
        };
        cat![export, body]
    }

    /// `import … from "…";`, or after `export`: a re-export or local export list.
    fn import(&mut self, import: &Import, exported: bool) -> Doc {
        let from = text(string_literal(slice(self.src, import.from_span)));
        let keyword = if exported { "" } else { "import " };
        if let Some(ns) = &import.namespace {
            return cat![keyword, "* as ", ns.name.clone(), " from ", from, ";"];
        }
        if import.all {
            return cat![keyword, "* from ", from, ";"];
        }
        if import.names.is_empty() && !exported {
            return cat!["import ", from, ";"];
        }
        let all_types = !import.names.is_empty() && import.names.iter().all(|n| n.type_only);
        let names = self.list(
            &import.names,
            import.from_span.lo,
            |n| (n.name.span.lo, n.alias.as_ref().unwrap_or(&n.name).span.hi),
            |_, n| {
                let modifier = if n.type_only && !all_types {
                    "type "
                } else {
                    ""
                };
                match &n.alias {
                    Some(alias) => text(format!("{modifier}{} as {}", n.name.name, alias.name)),
                    None => text(format!("{modifier}{}", n.name.name)),
                }
            },
        );
        let types = if all_types { "type " } else { "" };
        let list = cat![keyword, types, delimited("{", names, "}", true)];
        if import.from.is_empty() {
            return cat![list, ";"];
        }
        cat![list, " from ", from, ";"]
    }

    /// `type X = T;` — a union too long for one line gets one `| member` per line.
    fn type_alias(&mut self, alias: &TypeAlias) -> Doc {
        let head = cat![
            "type ",
            alias.name.name.clone(),
            self.generic_params(&alias.generics),
            " ="
        ];
        let TypeExprKind::Union(members) = &alias.ty.kind else {
            return cat![head, " ", self.ty(&alias.ty), ";"];
        };
        let docs = members.iter().map(|m| self.ty_operand(m)).collect();
        group(cat![
            head,
            indent(cat![
                line(),
                if_break("| ", ""),
                join(&cat![line(), "| "], docs)
            ]),
            ";"
        ])
    }

    /// `const pattern: T = init` without the semicolon (also used by `for` initializers).
    pub(super) fn var_decl(&mut self, var: &VarDecl) -> Doc {
        cat![var.kind.keyword(), " ", self.declarator(var)]
    }

    /// `pattern: T = init`: a declaration without its keyword (`for (let a = 1, b = 2; …)`).
    pub(super) fn declarator(&mut self, var: &VarDecl) -> Doc {
        let mut lhs = self.pattern(&var.pattern);
        if let Some(ty) = &var.ty {
            lhs = cat![lhs, ": ", self.ty(ty)];
        }
        match &var.init {
            Some(init) => self.assignment(lhs, " =", init),
            None => lhs,
        }
    }
}
