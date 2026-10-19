//! Items: imports, functions, `declare function`, type aliases and variables, plus the blank-line
//! policy between top-level items. Nominal type declarations are in `decls`.

use velt_syntax::ast::{
    Augment, Import, Item, ItemKind, Namespace, TypeAlias, TypeExprKind, VarDecl, VarKind,
};

use super::lists::delimited;
use super::script::{blank_before, Top};
use super::Printer;
use crate::doc::{cat, group, hardline, if_break, indent, join, line, text, Doc};
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
            ItemKind::Function(f) => return self.fn_decl_with(f, "function ", export),
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
            ItemKind::Augment(a) => self.augment(a, item.span.hi),
            ItemKind::Namespace(n) => self.namespace(n, item.span.hi),
        };
        cat![export, body]
    }

    /// `declare global { … }` / `declare module "…" { … }` (the item ends at `end`).
    fn augment(&mut self, a: &Augment, end: u32) -> Doc {
        let head = match &a.module {
            None => text("declare global"),
            Some((_, span)) => cat![
                "declare module ",
                text(string_literal(slice(self.src, *span)))
            ],
        };
        if a.items.is_empty() {
            return cat![head, " {}"];
        }
        self.block_items(head, &a.items, end)
    }

    /// `[declare] namespace N { … }` (the item ends at `end`).
    fn namespace(&mut self, n: &Namespace, end: u32) -> Doc {
        let declare = if n.declare { "declare " } else { "" };
        let head = cat![declare, "namespace ", n.name.name.clone()];
        if n.items.is_empty() {
            return cat![head, " {}"];
        }
        self.block_items(head, &n.items, end)
    }

    /// `head { items }`, the block ending at `end`.
    fn block_items(&mut self, head: Doc, items: &[Item], end: u32) -> Doc {
        let entries: Vec<Top> = items.iter().map(Top::Item).collect();
        let body = self.lines(
            &entries,
            end.saturating_sub(1),
            Top::range,
            blank_before,
            |p, t| match t {
                Top::Item(i) => p.item(i),
                Top::Stmt(s) => p.stmt(s),
            },
        );
        cat![head, " {", indent(cat![hardline(), body]), hardline(), "}"]
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
        // An interface with an index signature, which the parser gives as an alias.
        let members = match &alias.ty.kind {
            TypeExprKind::IndexSig {
                fields,
                key,
                value,
                readonly,
            } => Some(super::types::index_sig_members(
                fields, key, value, *readonly,
            )),
            TypeExprKind::Callable { sigs, members } => {
                Some(super::types::callable_members(sigs, members))
            }
            _ => None,
        };
        if let Some(members) = members {
            if slice(self.src, alias.ty.span).starts_with("interface") {
                let body = self.index_sig_lines(&members, alias.ty.span.hi);
                let head = cat![
                    "interface ",
                    alias.name.name.clone(),
                    self.generic_params(&alias.generics)
                ];
                return cat![head, " ", super::decls::braced(body)];
            }
        }
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
        cat![self.var_keyword(var), " ", self.declarator(var)]
    }

    /// The keyword of `var`: `var` when the source has it (the AST reads `var` as `let`),
    /// also for a further declarator of a `var` (whose span starts at its pattern): the one
    /// before its comma was a `var` declarator. Decided from the declaration's own position.
    fn var_keyword(&mut self, var: &VarDecl) -> &'static str {
        if var.kind != VarKind::Let {
            return var.kind.keyword();
        }
        let written = crate::source::slice(self.src, var.span);
        let starts = |kw: &str| {
            written
                .strip_prefix(kw)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace()))
        };
        let is_var = starts("var") || (!starts("let") && self.follows_var(var.span.lo));
        if is_var {
            self.var_ends.insert(var.span.hi);
            "var"
        } else {
            "let"
        }
    }

    /// Is the declarator starting at `lo` a further declarator (after a comma) of a `var`?
    fn follows_var(&self, lo: u32) -> bool {
        let before = self.src[..lo as usize].trim_end();
        let Some(before) = before.strip_suffix(',') else {
            return false;
        };
        let prev_end = before.trim_end().len() as u32;
        self.var_ends.contains(&prev_end)
    }

    /// `pattern: T = init`: a declaration without its keyword (`for (let a = 1, b = 2; …)`).
    pub(super) fn declarator(&mut self, var: &VarDecl) -> Doc {
        let mut lhs = self.pattern(&var.pattern);
        // `let x!: T` (TypeScript's definite assignment assertion; the AST does not keep it).
        if var.ty.is_some()
            && crate::source::chars_after(self.src, var.pattern.span.hi) == (Some('!'), Some(':'))
        {
            lhs = cat![lhs, "!"];
        }
        if let Some(ty) = &var.ty {
            lhs = cat![lhs, ": ", self.ty(ty)];
        }
        match &var.init {
            Some(init) => self.assignment(lhs, " =", init),
            None => lhs,
        }
    }
}
