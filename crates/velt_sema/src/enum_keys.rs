//! Enum members as member names (#923): `[ReactiveFlags.SKIP]?: boolean` in an interface, an
//! object type or a class, `{ [Flags.SKIP]: true }` and `o[Flags.SKIP]` name the member whose
//! name is the member's value, `"__v_skip"` for `SKIP = "__v_skip"`, or the number as `String`
//! spells it for a numeric enum (`[Kind.A]` is `"0"`), as in TypeScript. The value is read from
//! the enum's declaration, so a key resolves before the enum's members are collected.

use crate::collect::shapes::{const_int, const_str};
use crate::ctx::{Ctx, Item};

impl Ctx<'_> {
    /// The member name `Enum.Member` written in `module` stands for, if `Enum` is an enum
    /// declared in the program and `Member` one of its members.
    pub(crate) fn enum_key(&self, module: usize, path: &str) -> Option<String> {
        let (enum_name, member) = path.split_once('.')?;
        let Some(Item::Def(d)) = self.lookup_item(module, enum_name) else {
            return None;
        };
        let decl = self.enum_info(d).filter(|e| !e.is_union)?.decl?;
        let is_string = (decl.variants.iter()).any(|v| {
            v.discriminant
                .as_ref()
                .is_some_and(|e| const_str(e).is_some())
        });
        let mut next = 0i64;
        for v in &decl.variants {
            let value = match (&v.discriminant, is_string) {
                (Some(e), true) => const_str(e)?,
                (None, true) => return None,
                (Some(e), false) => {
                    next = const_int(e)?;
                    next.to_string()
                }
                (None, false) => next.to_string(),
            };
            if v.name.name == member {
                return (!crate::reserved_key(&value)).then_some(value);
            }
            next = next.wrapping_add(1);
        }
        None
    }
}
