//! The encoding of this module's types in a finished unit (`units::codec`): every field and
//! variant, so a new one fails to build.

use super::*;

crate::units::codec::structs! {
    FnSites {
        seeds, sites, seen, by_file, found, quiet, grew, sites_grew, indirect, indirect_sites,
        written, target, push, param_types, value_args, async_ret, promise_var_at, generator_ret,
        rejects, anon_written, alias_written, async_args, closure_return, closure_rejects,
        closure_sites, rest_closures, flows, added, last, use_vars, user_vars, var_of, trap_vars,
        vars, solved_anons, solved, var_sets, stored, thrown, reject_values, sets_empty,
        site_closures, closure_defs, structural, var_values, any_calls, sort_cmps,
        any_call_srcs,
    }
    FnTypeSeeds {
        sites, hints, slots, no_vars, proxy_sites, exact, closure_targets, methods,
        any_calls_throw, any_sorts,
    }
}
crate::units::codec::enums! {
    @fielded Portable {
        0 => Leaf(f0),
        1 => Def(f0, f1, f2),
        2 => Dyn(f0, f1, f2),
        3 => Array(f0, f1),
        4 => Option(f0, f1),
    }
}

/// `Portable` is listed `@fielded`: it ends from zeros, so damaged input ends in it.
#[cfg(test)]
#[test]
fn portable_ends_on_damaged_input() {
    assert!(crate::units::codec::ends_from_zeros::<Portable>());
}
