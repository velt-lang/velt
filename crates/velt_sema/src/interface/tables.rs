//! The program-wide facts of a summary: every field of `hir::Program`, destructured without `..`
//! so that a new field fails to build here until it is hashed or skipped with a reason.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::hashing::{finish, unordered, Hashing};
use crate::hir::{self, DecoratorTarget};

/// Each kind of fact and its hash, in a fixed order.
pub(super) fn facts(h: &mut Hashing) -> Vec<(&'static str, u64)> {
    let hir::Program {
        types: _,               // Hashed where a type is used, by structure.
        defs,                   // Functions per function; the rest as declarations.
        entry,                  // "entry"
        impls,                  // "implementations"
        anon_shapes,            // "type tables"
        union_shapes,           // "type tables"
        counted_objects,        // "counted types"
        null_proto_records,     // "flags"
        record_fills,           // "flags"
        fn_shapes: _,           // Per function, through its span (`Hashing::function`).
        module_state,           // "module state"
        type_ops,               // "type tables"
        any_impl,               // "implementations"
        unknown,                // "implementations"
        object,                 // "implementations"
        function,               // "implementations"
        super_calls: _,         // Spans of calls in bodies: read when that body is lowered.
        dyn_members,            // "dynamic members"
        array_methods_open,     // "dynamic members"
        any_call_names,         // "dynamic members"
        generator_throw,        // "flags"
        nothing_slots,          // "flags"
        upcasts,                // "implementations"
        upcast_impls,           // "implementations"
        cpu_targets: _,         // Per function.
        type_names: _,          // Names for messages only.
        generic_dyn_methods,    // "dynamic members"
        arg_free_generic_fns,   // "dynamic members"
        fresh_closures: _,      // Per function.
        extension_impls,        // "implementations"
        dyn_bounds,             // "implementations"
        lock_free_callbacks: _, // Per function.
        unchecked,              // "reached std"
        hooks_demanded,         // "flags"
        live: _,                // Debug compilers only: what lazy std reached, as `unchecked` says.
        any_fn_views,           // "dynamic members"
        generated,              // "generated instances"
        decorators,             // "decorators"
        c_exports,              // "decorators"
        fn_wrappers: _,         // Per function.
    } = h.p;
    let declarations = (defs.iter()).filter_map(|d| h.declaration(d)).collect();
    let declarations = unordered(declarations);
    // An implementation stands for itself by its content: its index moves.
    let imp = |h: &mut Hashing, i: u32| -> u64 {
        let Some(hir::ImplDef {
            ty,
            generics,
            iface,
            iface_args,
            methods,
        }) = impls.get(i as usize)
        else {
            return 0;
        };
        let head = (h.ty(*ty), generics, h.name(*iface));
        finish((head, h.tys(iface_args), h.names(methods.iter().copied())))
    };
    let mut all: Vec<u64> = (0..impls.len() as u32).map(|i| imp(h, i)).collect();
    all.sort_unstable();
    let mut ups: Vec<_> = (upcasts.iter())
        .map(|(i, ts)| (h.name(*i), h.names(ts.iter().copied())))
        .collect();
    ups.sort_unstable();
    let mut up_impls: Vec<_> = (upcast_impls.iter())
        .map(|(i, js)| {
            (
                imp(h, *i),
                js.iter().map(|j| imp(h, *j)).collect::<Vec<_>>(),
            )
        })
        .collect();
    up_impls.sort_unstable();
    let mut ext: Vec<_> = (extension_impls.iter())
        .map(|(i, bounds)| {
            let bounds: Vec<_> = (bounds.iter())
                .map(|hir::ImplBound { param, iface, args }| (param, h.name(*iface), h.tys(args)))
                .collect();
            (imp(h, *i), finish(bounds))
        })
        .collect();
    ext.sort_unstable();
    let mut bounds: Vec<_> = (dyn_bounds.iter())
        .map(|(i, bs)| {
            let bs: Vec<_> = bs
                .iter()
                .map(|(b, args)| (h.name(*b), h.tys(args)))
                .collect();
            (h.name(*i).to_string(), finish(bs))
        })
        .collect();
    bounds.sort_unstable();
    let any = (
        any_impl.map(|i| imp(h, i)),
        [unknown, object, function].map(|d| h.opt_name(*d)),
    );
    let implementations = finish((all, ups, up_impls, ext, bounds, any));

    let anon = (anon_shapes.iter())
        .map(|(fields, d)| {
            let fields: Vec<_> = (fields.iter())
                .map(|(name, t, optional, presence)| (name, h.ty(*t), optional, presence))
                .collect();
            (h.name(*d).to_string(), finish(fields))
        })
        .collect();
    let unions = (union_shapes.iter())
        .map(|(ts, d)| (h.name(*d).to_string(), finish(h.tys(ts))))
        .collect();
    let mut ops: Vec<_> = type_ops.iter().map(|(a, b)| (h.ty(*a), h.ty(*b))).collect();
    ops.sort_unstable();
    let type_tables = finish((unordered(anon), unordered(unions), ops));

    let mut counted = h.tys(counted_objects);
    counted.sort_unstable();
    let state = module_state.as_ref().map(|m| {
        let hir::ModuleState {
            ty,
            slots,
            deferred,
        } = m;
        (h.ty(*ty), h.names(slots.iter().copied()), deferred)
    });
    // As a set: two stubs may share a name.
    let mut reached = h.sorted_names(unchecked.iter().copied());
    reached.dedup();
    let generated = (generated.iter())
        .map(|((f, args), g)| (h.name(*g).to_string(), finish((h.name(*f), h.tys(args)))))
        .collect();

    let members = dyn_members.as_ref().map(|m| {
        let items = (m.iter())
            .map(|(d, members)| {
                let members: Vec<_> = (members.iter())
                    .map(
                        |hir::DynMember {
                             name,
                             kind,
                             def,
                             owner,
                         }| {
                            (name, format!("{kind:?}"), h.name(*def), h.ty(*owner))
                        },
                    )
                    .collect();
                (h.name(*d).to_string(), finish(members))
            })
            .collect();
        unordered(items)
    });
    let mut calls = any_call_names.clone();
    calls.sort_unstable();
    let mut methods: Vec<_> = generic_dyn_methods.iter().collect();
    methods.sort_unstable();
    let arg_free = h.sorted_names(arg_free_generic_fns.iter().copied());
    let views = h.sorted_names(any_fn_views.iter().copied());
    let dynamic = finish((members, array_methods_open, calls, methods, arg_free, views));

    let uses = (decorators.iter())
        .map(|(target, uses)| {
            let (tag, d, i) = match *target {
                DecoratorTarget::Def(d) => (0u8, d, 0),
                DecoratorTarget::Field(d, i) => (1, d, i),
                DecoratorTarget::Param(d, i) => (2, d, i),
            };
            let mut s = DefaultHasher::new();
            for hir::Decorator { def, args, span: _ } in uses {
                h.name(*def).hash(&mut s);
                args.len().hash(&mut s);
                args.iter().for_each(|a| h.tree(a, &mut s));
            }
            (format!("{tag}:{}:{i}", h.name(d)), s.finish())
        })
        .collect();
    let exports: Vec<_> = (c_exports.iter())
        // `CTy` holds no ids.
        .map(
            |hir::CExport {
                 def,
                 name,
                 params,
                 ret,
                 span: _,
             }| { (h.name(*def), name, format!("{params:?} {ret:?}")) },
        )
        .collect();
    let decorators = finish((unordered(uses), exports));

    let bits = (
        null_proto_records,
        record_fills,
        generator_throw,
        nothing_slots,
    );
    let flags = finish((bits, format!("{hooks_demanded:?}")));
    vec![
        ("declarations", declarations),
        ("implementations", implementations),
        ("type tables", type_tables),
        ("counted types", finish(counted)),
        ("module state", finish(state)),
        ("entry", finish(h.opt_name(*entry))),
        ("reached std", finish(reached)),
        ("generated instances", unordered(generated)),
        ("dynamic members", dynamic),
        ("decorators", decorators),
        ("flags", flags),
    ]
}
