//! Find references and rename, from sema's reference table (`velt_sema::ide::Analysis::references`),
//! across every module of the document's program.
//!
//! Rename is offered for locals, parameters and items (functions, types, constants) declared
//! outside the standard library. Uses through an import alias keep the alias (only spans spelled
//! like the old name are edited).

use velt_common::Span;
use velt_sema::ide::DefKind;

use crate::analysis::Analysis;
use crate::sema_query;

/// Spans naming the definition at `offset` (its declaration only if `include_declaration`).
pub fn references(analysis: &Analysis, offset: u32, include_declaration: bool) -> Vec<Span> {
    let (Some(ide), Some(def)) = (analysis.ide.as_ref(), sema_query::def_at(analysis, offset))
    else {
        return vec![];
    };
    ide.references(&def)
        .into_iter()
        .filter(|s| include_declaration || *s != def.span)
        .collect()
}

/// Spans to replace with `new_name` to rename the definition at `offset`, or why it cannot be
/// renamed.
pub fn rename(analysis: &Analysis, offset: u32, new_name: &str) -> Result<Vec<Span>, String> {
    let def = sema_query::def_at(analysis, offset).ok_or("there is no symbol to rename here")?;
    let renamable = matches!(
        def.kind,
        DefKind::Local
            | DefKind::Parameter
            | DefKind::Function
            | DefKind::Struct
            | DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::TypeAlias
            | DefKind::Constant
    );
    if !renamable || def.name == "this" {
        return Err(format!("`{}` cannot be renamed", def.name));
    }
    if analysis.is_std(def.module) {
        return Err(format!(
            "`{}` is declared in the standard library",
            def.name
        ));
    }
    if !is_identifier(new_name) {
        return Err(format!("`{new_name}` is not a valid identifier"));
    }
    let ide = analysis
        .ide
        .as_ref()
        .ok_or("the program has not been analyzed")?;
    Ok(ide
        .references(&def)
        .into_iter()
        .filter(|s| analysis.snippet(*s) == def.name)
        .collect())
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let head = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$');
    head && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !crate::completion::is_keyword(name)
}
