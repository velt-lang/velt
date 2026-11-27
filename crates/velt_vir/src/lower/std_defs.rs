//! How lowering finds std's definitions: the hooks of velt_common's table (`std_hook!`) and the
//! few other std items it names (`std_item!`), all by qualified name (#2197).
//!
//! A hook is in a program in one of three ways, and lowering asks for it through the function
//! that says which one it needs:
//! - [`Cx::std_hook`]: lowering cannot do without it, so it is an internal error when std lacks it.
//! - [`Cx::std_hook_if_loaded`]: the feature it implements is in the program only when its module
//!   is (a program without `Proxy` has no array prototype to read). Its module not loaded, the
//!   feature is off; its module loaded *without* it (a renamed or removed hook) is an internal
//!   error naming both, never a silent switch to other code.
//! - [`Cx::std_hook_live`]: as `std_hook_if_loaded`, and only where sema checked it (lazy std
//!   leaves a hook unchecked where no live code needs it: velt_sema `lazy_std`).
//!
//! A module loaded under a second name (#3731: `std/../../…/std/lang/hooks/sched_all`, a path
//! that reached std by another spelling) has its definitions under that name, so its canonical
//! module is not loaded. A hook missing under its canonical name whose item is there under such a
//! name (a loaded module whose path ends in `/` and the canonical module) is an internal error
//! naming both, in each of the three, and not a feature switched off (#3745).

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::{self, DefId};

use super::{ice, Cx};

/// std's definitions by qualified name, and the modules they come from.
#[derive(Default)]
pub(super) struct StdDefs<'h> {
    /// The first function or class of each name, as a scan finds it.
    by_name: FxHashMap<&'h str, DefId>,
    /// The modules (`std/lang/hooks/array_view`) with at least one function or class here.
    modules: FxHashSet<&'h str>,
    /// Modules loaded under a second name: the canonical std module (`std/lang/hooks/sched_all`)
    /// to the name it has here (`std/../../…/std/lang/hooks/sched_all`).
    second_names: FxHashMap<&'h str, &'h str>,
}

/// The canonical std module whose second name `module` may be: what follows its last `/std/`
/// (`std/../x/std/lang/a` → `std/lang/a`).
fn canonical_std_module(module: &str) -> Option<&str> {
    let at = module.rfind("/std/")?;
    Some(&module[at + 1..])
}

impl<'h> Cx<'h> {
    fn std_defs(&self) -> &StdDefs<'h> {
        let hir: &'h hir::Program = self.hir;
        self.prelude_defs.get_or_init(|| {
            let mut defs = StdDefs::default();
            for (i, d) in hir.defs.iter().enumerate() {
                let name = match d {
                    hir::Def::Fn(f) => f.name.as_str(),
                    hir::Def::Adt(a) => a.name.as_str(),
                    _ => continue,
                };
                defs.by_name.entry(name).or_insert(DefId(i as u32));
                if let Some((module, _)) = name.split_once("::") {
                    if defs.modules.insert(module) {
                        if let Some(canonical) = canonical_std_module(module) {
                            defs.second_names.entry(canonical).or_insert(module);
                        }
                    }
                }
            }
            defs
        })
    }

    /// The function or class named `name` (qualified: `std/lang/hooks/array_view::ArrayView`),
    /// if the program has one. Lowering asks for a hook through [`Self::std_hook`],
    /// [`Self::std_hook_if_loaded`] or [`Self::std_hook_live`], which say what its absence means.
    fn prelude_def(&self, name: &str) -> Option<DefId> {
        self.std_defs().by_name.get(name).copied()
    }

    /// Does the program load the module of the qualified name `name`?
    fn std_module_loaded(&self, name: &str) -> bool {
        let module = name.split_once("::").map_or(name, |(m, _)| m);
        self.std_defs().modules.contains(module)
    }

    /// Hook `name`, missing under its canonical name, found under a second name of its module:
    /// an internal error naming both.
    fn under_second_name(&self, name: &str) {
        let Some((module, item)) = name.split_once("::") else {
            return;
        };
        let defs = self.std_defs();
        let Some(second) = defs.second_names.get(module) else {
            return;
        };
        let found = format!("{second}::{item}");
        if defs.by_name.contains_key(found.as_str()) {
            ice(format_args!(
                "std hook `{name}` missing, found as `{found}`: its module is loaded under a \
                 second name"
            ));
        }
    }

    /// std's definition `name` (a qualified name from `std_hook!` or `std_item!`), which lowering
    /// cannot do without.
    pub(super) fn std_hook(&self, name: &str) -> DefId {
        self.prelude_def(name).unwrap_or_else(|| {
            self.under_second_name(name);
            ice(format_args!("std hook `{name}` missing"))
        })
    }

    /// Hook `name` where the program loads its module; `None` where it doesn't (the feature is
    /// not in the program). Its module loaded without it, or the hook found only under a second
    /// name of its module, is an internal error.
    pub(super) fn std_hook_if_loaded(&self, name: &str) -> Option<DefId> {
        match self.prelude_def(name) {
            Some(d) => Some(d),
            None if self.std_module_loaded(name) => ice(format_args!(
                "std hook `{name}` missing from its module, which the program loads"
            )),
            None => {
                self.under_second_name(name);
                None
            }
        }
    }

    /// [`Self::std_hook_if_loaded`], and `None` too where sema left it unchecked.
    pub(super) fn std_hook_live(&self, name: &str) -> Option<DefId> {
        self.std_hook_if_loaded(name)
            .filter(|d| !self.hir.unchecked.contains(d))
    }
}
