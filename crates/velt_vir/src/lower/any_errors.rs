//! The `TypeError`s of checked conversions out of `any` (any.rs): the expected type named as
//! TypeScript writes it, and what the value is instead.

use velt_sema::hir::{AdtKind, DefId, TyId, TyKind};

use super::any::{FOUND, TAG_OBJECT};
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// The name of `ty` in a `TypeError`, with its type arguments (`Box<number>`).
    pub(super) fn any_target_name(&mut self, ty: TyId) -> String {
        if let TyKind::Array(e) = self.cx.kind(ty) {
            let e = self.any_target_name(e);
            return match e.contains(' ') {
                true => format!("({e})[]"),
                false => format!("{e}[]"),
            };
        }
        if let TyKind::Option(x, n) = self.cx.kind(ty) {
            return format!("{}{}", self.any_target_name(x), n.suffix());
        }
        if let TyKind::Adt(d, args) = self.cx.kind(ty) {
            if self.cx.adt_def(d).kind == AdtKind::Anon {
                return self.object_type_name(d, &args);
            }
        }
        let name = self.cx.type_name(ty);
        match self.cx.kind(ty) {
            TyKind::Adt(_, args) if !args.is_empty() => {
                let args: Vec<String> = args.iter().map(|a| self.any_target_name(*a)).collect();
                format!("{name}<{}>", args.join(", "))
            }
            _ => name,
        }
    }

    /// An object type as TypeScript writes it (`{ n: number; tag?: string }`), its field types
    /// at the type arguments `args`.
    fn object_type_name(&mut self, d: DefId, args: &[TyId]) -> String {
        let fields: Vec<(String, TyId, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.optional))
            .collect();
        let parts: Vec<String> = fields
            .into_iter()
            .map(|(name, t, optional)| {
                let t = match args.is_empty() {
                    true => t,
                    false => self.cx.subst(t, args),
                };
                let t = match (optional, self.cx.kind(t)) {
                    (true, TyKind::Option(x, _)) => x,
                    _ => t,
                };
                let q = if optional { "?" } else { "" };
                format!("{name}{q}: {}", self.any_target_name(t))
            })
            .collect();
        match parts.is_empty() {
            true => "{}".to_string(),
            false => format!("{{ {} }}", parts.join("; ")),
        }
    }

    /// Stop with `TypeError: the value is not <ty> (it is <found>)`.
    pub(super) fn any_type_error(&mut self, desc: Operand, ty: TyId) {
        let want = self.any_target_name(ty);
        let an = article(&want);
        // A value of the expected generic class (or an array) with other type arguments is
        // named as one: "it is a Map", "it is an array".
        let (same_id, same_name) = match self.cx.kind(ty) {
            TyKind::Array(_) => (Some(super::any_set_site::ARRAY_ID), "an array".to_string()),
            TyKind::Adt(d, args)
                if self.cx.is_class(ty) && !args.is_empty() && !self.cx.has_header(d) =>
            {
                let name = self.cx.type_name(ty);
                (
                    Some(self.cx.class_range(d).0),
                    format!("{} {name}", article(&name)),
                )
            }
            _ => (None, String::new()),
        };
        let tag = self.any_tag_of(desc.clone());
        let blocks: Vec<_> = (0..FOUND.len()).map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(k, b)| (k as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: blocks[TAG_OBJECT as usize],
        });
        for (k, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            if let (Some(id), true) = (same_id, k == TAG_OBJECT as usize) {
                let (yes, no) = (self.new_block(), self.new_block());
                let have = self.vtable_class_id(desc.clone());
                let want_id = cint(id as i128, Ty::U64);
                let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, have, want_id));
                self.branch(same, yes, no);
                self.switch_to(yes);
                self.panic_msg(&format!(
                    "TypeError: the value is not {an} {want} (it is {same_name})"
                ));
                self.terminate(Terminator::Unreachable);
                self.switch_to(no);
            }
            let mut msg = format!(
                "TypeError: the value is not {an} {want} (it is {})",
                FOUND[k]
            );
            if k == TAG_OBJECT as usize && self.prelude_record(ty).is_some() {
                // Node reads any object as a record; Velt's objects of a fixed shape are not one.
                msg.push_str(": only a Record converts to a Record (an object of a fixed shape does not yet, #1219)");
            }
            self.panic_msg(&msg);
            self.terminate(Terminator::Unreachable);
        }
    }
}

/// The indefinite article before `word` ("an Animal", "a Map").
fn article(word: &str) -> &'static str {
    match word.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}
