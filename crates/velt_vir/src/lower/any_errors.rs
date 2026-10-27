//! The `TypeError`s of checked conversions out of `any` (any.rs): the expected type named as
//! TypeScript writes it, and what the value is instead.

use velt_sema::hir::{AdtKind, Def, DefId, LitValue, TyId, TyKind};

use super::any::{FOUND, TAG_OBJECT};
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// The name of `ty` in a `TypeError`, as TypeScript writes it (`Box<number>`,
    /// `(arg0: number) => string`, `{ n: number }`, a type alias's name).
    pub(super) fn any_target_name(&mut self, ty: TyId) -> String {
        self.ts_name(ty, &mut vec![])
    }

    /// [`any_target_name`](Self::any_target_name); `open` holds the object types being
    /// written, so a type reaching itself through its fields is written `...` there, as tsc
    /// cuts a type it cannot write out.
    fn ts_name(&mut self, ty: TyId, open: &mut Vec<DefId>) -> String {
        if let Some(n) = self.cx.hir.type_names.get(&ty) {
            return n.clone();
        }
        let list = |lw: &mut Self, ts: &[TyId], open: &mut Vec<DefId>| {
            let parts: Vec<String> = ts.iter().map(|t| lw.ts_name(*t, open)).collect();
            parts.join(", ")
        };
        match self.cx.kind(ty) {
            TyKind::Array(e) => {
                let e = self.ts_name(e, open);
                match e.contains(" | ") || e.contains("=>") {
                    true => format!("({e})[]"),
                    false => format!("{e}[]"),
                }
            }
            TyKind::Option(x, n) => {
                let x = self.ts_name(x, open);
                match x.contains("=>") {
                    true => format!("({x}){}", n.suffix()),
                    false => format!("{x}{}", n.suffix()),
                }
            }
            TyKind::Tuple(ts) => format!("[{}]", list(self, &ts, open)),
            TyKind::Map(k, v) => format!("Map<{}>", list(self, &[k, v], open)),
            TyKind::Promise(t, _) => format!("Promise<{}>", self.ts_name(t, open)),
            TyKind::Unit => "void".to_string(),
            TyKind::Never => "never".to_string(),
            TyKind::Literal(v) => match v {
                LitValue::Str(s) => format!("{s:?}"),
                LitValue::Bool(b) => b.to_string(),
                LitValue::Int(_, i) => i.to_string(),
                _ => self.cx.type_name(ty),
            },
            TyKind::FnPtr {
                params, ret, rest, ..
            } => {
                let last = params.len().saturating_sub(1);
                let ps: Vec<String> = (params.iter().enumerate())
                    .map(|(i, p)| {
                        let t = self.ts_name(*p, open);
                        match rest && i == last {
                            true => format!("...args: {t}"),
                            false => format!("arg{i}: {t}"),
                        }
                    })
                    .collect();
                format!("({}) => {}", ps.join(", "), self.ts_name(ret, open))
            }
            TyKind::Adt(d, args) if self.is_anon(d) => {
                if open.contains(&d) {
                    return "...".to_string();
                }
                open.push(d);
                let shown = self.object_type_name(d, &args, open);
                open.pop();
                shown
            }
            TyKind::Adt(_, args) | TyKind::Dyn(_, args) if !args.is_empty() => {
                let name = self.cx.type_name(ty);
                format!("{name}<{}>", list(self, &args, open))
            }
            _ => self.cx.type_name(ty),
        }
    }

    /// Is `d` an anonymous object type's definition?
    fn is_anon(&self, d: DefId) -> bool {
        matches!(self.cx.hir.def(d), Def::Adt(a) if a.kind == AdtKind::Anon)
    }

    /// An object type as TypeScript writes it (`{ n: number; tag?: string }`), its field types
    /// at the type arguments `args`.
    fn object_type_name(&mut self, d: DefId, args: &[TyId], open: &mut Vec<DefId>) -> String {
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
                format!("{name}{q}: {}", self.ts_name(t, open))
            })
            .collect();
        match parts.is_empty() {
            true => "{}".to_string(),
            false => format!("{{ {} }}", parts.join("; ")),
        }
    }

    /// The `AnyTo` text being lowered, followed by `: ` (empty without one).
    pub(super) fn any_context_words(&self) -> String {
        self.any_context
            .as_ref()
            .map_or(String::new(), |c| format!("{c}: "))
    }

    /// Stop with `TypeError: the value is not <ty> (it is <found>)`, after the `AnyTo` text
    /// being lowered when there is one.
    pub(super) fn any_type_error(&mut self, desc: Operand, ty: TyId) {
        let want = self.any_target_name(ty);
        let an = article_of(&want);
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
                let words = self.any_context_words();
                self.panic_msg(&format!(
                    "TypeError: {words}the value is not {an} {want} (it is {same_name})"
                ));
                self.terminate(Terminator::Unreachable);
                self.switch_to(no);
            }
            let mut msg = format!(
                "TypeError: {}the value is not {an} {want} (it is {})",
                self.any_context_words(),
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

/// The words before the type name `want` in a `TypeError`: its article, or "an object" before
/// an object type ("an object { n: number }").
pub(super) fn article_of(want: &str) -> &'static str {
    match want.starts_with('{') {
        true => "an object",
        false => article(want),
    }
}

/// The indefinite article before `word` ("an Animal", "a Map").
fn article(word: &str) -> &'static str {
    match word.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}
