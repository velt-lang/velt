//! What lowering reads of `Program::decorators` (hir_encodings.md "Decorators"): the compile-time
//! decorators the compiler acts on, each found by its role in the hook table
//! (`velt_common::std_hooks`), as sema finds them (`velt_sema` `decorators/known.rs`), never by
//! the name a use is written with.

use velt_common::std_hook;
use velt_sema::hir::{self, DefId};

use super::Cx;
use crate::vir::Inline;

impl Cx<'_> {
    /// `@inline` / `@noInline` on function `def` (`Function::inline`): sema has checked that at
    /// most one is there, on a function that is neither async nor a generator. `@inline` is
    /// `Always` when it says `"always"`; its default, `"hint"`, offers the body to callers, which
    /// every body is while a program is one unit, so it is `Default` until units (#2943) carry
    /// bodies across (docs/internals/design/package-machine-code.md §1).
    pub(super) fn inline_of(&self, def: DefId) -> Inline {
        let uses = self.hir.decorators.get(&hir::DecoratorTarget::Def(def));
        for u in uses.map_or(&[][..], Vec::as_slice) {
            if let hir::Def::ExternFn(x) = self.hir.def(u.def) {
                match x.name.as_str() {
                    std_hook!(decorator_inline) => {
                        return match u.args.first() {
                            Some(hir::ConstTree::Str(how)) if how == "always" => Inline::Always,
                            _ => Inline::Default,
                        };
                    }
                    std_hook!(decorator_no_inline) => return Inline::Never,
                    _ => {}
                }
            }
        }
        Inline::Default
    }
}
