//! A finished unit's checking context, encoded whole (#2943 M2): [`encode_unit`] and
//! [`decode_unit`]. What a later unit's stage reads of the context is saved (definitions, types,
//! scopes, interning tables and the side tables of the passes); what is a memo, state of one
//! check or one pass, or the app's alone is not, and a decoded
//! context has it as [`Ctx::over`] makes it. The context is destructured field by field
//! ([`unit_fields!`]), so a field added to `Ctx` fails to compile until it is saved or skipped
//! here.
//!
//! The encoding: [`FORMAT`], the unit's file table (each file as the module it is the file of),
//! the AST fragments ([`super::Frags`]), then the fields in order. The modules themselves are
//! the caller's ([`decode_unit`]). The definition tables ([`DefTable`]) keep where each entry
//! is, and a decoded unit decodes an entry when the check first reads it.

use velt_common::{Diagnostic, FileId, Fix, Label, Severity};

use super::{Arena, Codec, Reader, Writer};
use crate::ctx::Ctx;
use crate::hir::{TyId, TyTable};
use crate::types::Types;
use crate::SourceModule;

/// The version of the encoding: the first bytes of every encoded unit, before the checksum of
/// the rest ([`checksum`]).
pub(crate) const FORMAT: &[u8; 8] = b"vunit\x00\x00\x06";

/// The fields of `Ctx` a unit saves (`save`), in order, then those it does not: `skip` (the
/// caller's, what every check installs again), `memo` (built again on demand: one built over a
/// unit's definitions alone is stale once a later unit's are added, [`clear_memos`]) and
/// `empty` (state of one pass, or the app's alone: at a unit's end as `Ctx::over` makes it,
/// which a test checks, [`empty_fields_differing`]). Generates the field walk of the encoder and
/// decoder.
macro_rules! unit_fields {
    (
        save: [$($p:ident),* $(,)?],
        special: [$($sp:ident),* $(,)?],
        skip: [$($s:ident),* $(,)?],
        memo: [$($m:ident),* $(,)?],
        empty: [$($e:ident),* $(,)?] $(,)?
    ) => {
        /// Drop the memos of `cx` (a finished unit's): a later unit's stage builds them again
        /// over its own definitions too, as it does from a unit loaded from disk.
        pub(crate) fn clear_memos(cx: &mut Ctx) {
            $(cx.$m = Default::default();)*
        }

        /// Encode the saved fields of `cx`, with the bytes each took.
        fn enc_fields(cx: &Ctx, w: &mut Writer, sizes: &mut Vec<(&'static str, usize)>) {
            let Ctx { $($p,)* $($sp,)* $($s: _,)* $($m: _,)* $($e: _,)* } = cx;
            $(
                let at = w.out.len();
                Codec::enc($p, w);
                sizes.push((stringify!($p), w.out.len() - at));
            )*
            $(
                let at = w.out.len();
                special::enc(stringify!($sp), $sp, w);
                sizes.push((stringify!($sp), w.out.len() - at));
            )*
        }

        fn dec_fields(cx: &mut Ctx, r: &mut Reader) {
            $(
                cx.$p = Codec::dec(r);
            )*
            $(special::dec(stringify!($sp), &mut cx.$sp, r);)*
        }

        /// The `empty` fields of a finished unit `cx` that are not as in `fresh` (a context
        /// `Ctx::over` made): name, then each one's shape ([`Fresh`]).
        #[cfg(test)]
        pub(super) fn empty_fields_differing(cx: &Ctx, fresh: &Ctx) -> Vec<(&'static str, String, String)> {
            let mut out = vec![];
            $(
                let (a, b) = (Fresh::shape(&cx.$e), Fresh::shape(&fresh.$e));
                if a != b {
                    out.push((stringify!($e), a, b));
                }
            )*
            out
        }
    };
}

unit_fields! {
    save: [
        fn_shapes, ty, defs, info, def_spans, scopes, prelude, prelude_type_keys, unit_sums,
        aliases, extensions, impls, iface_writes, part_ifaces, synth_ifaces, generic_fns,
        generic_called, union_views, any, ext_impls, ext_by_iface, anon, readonly_twins,
        array_twins,
        any_unknown_views, objects, brands, brand_keys, generic_brands, unique_symbols,
        record_views, field_only, field_only_of, primitive_guards, writes_undefined,
        nothing_slots, shaped, laid_out, counted_objects, changes_held, lock_free_callbacks,
        module_scripts, state_refused, lazy, unions, recursive_unions, generic_recursive,
        recursive_union_insts, alias_names, generic_overrides, getter_only_setters,
        abstract_methods, defined_setters, defaults_checked, fills, default_throws, object_impls,
        hook_adapters, field_defaults_checked, closure_counts, fn_defs, fn_values, frozen_reads,
        cpu_targets, provisional_slots, literal_ifaces, literal_structs, iface_adapters,
        dyn_iface_targets, fn_views, fn_sites, promise_forwarders, nested, nested_locals,
        decorator_uses, c_exports, enclosing_class, static_this, static_copies, static_copy_of,
        generator_values, overload_sets, callback_wrappers, kept_fns, kept_consts, symbol_keys,
        held_borrows_used, object_copies, method_facts, super_calls, type_defaults, diags,
        widening_values, recursive_union_instances, js_api_modules, glue_bodies, method_writes,
    ],
    // Saved in part (`special`).
    special: [sigq, type_level],
    skip: [
        // The caller's (`decode_unit`), or set again by every check that uses the unit.
        modules, root, gen, units, import_targets, loads,
        loads_added, unit_base, units_stage,
        stack_base, stack_budget, held_borrows, ide, body_records, source_facts, std_missed,
        kept_demand,
        // What the source rewrites found, installed again by every check.
        promoted_consts, asserted_rmw, global_props, generic_arrow_fns, generic_arrow_all,
        unsafe_arrow_fns, alias_blocked, kept_nested, closure_groups, group_names,
        has_overloads, gen_throw, overloaded_methods, ns_private, global_namespaces, recovered,
        // Set again by the app's collection (`collect::collect_app`).
        shapes_done, bodies_ok, collected_defs,
        // std's own: read or taken only by std's bodies' check and std's passes after the
        // bodies, never by a later unit's (whose passes look at their unit's functions,
        // `Ctx::unit_fns`). A unit is saved once std is a check of its own, after those passes.
        thread_local_uses, throw_checks, loop_roots, closure_defs, rec,
    ],
    memo: [
        marked_aliases, extension_names, impl_index, ext_memo, ext_exists_memo, ext_memo_impls,
        canon_memo, canon_errors, canon_keys, primitive_statics, classes_below, groups,
        shared_memo, pure_fns, subclasses, native_libraries,
        decorator_scopes, export_memo,
    ],
    empty: [
        // State within one pass or one body's check, empty when a unit is finished.
        bad_key_sites, closure_constraints, adapter_uses, in_blanket, impl_scope,
        narrowed_fields, query_locals, checking_unused_aliases, shaping,
        assigned_objects, replaced_unions, module_slots, global_cycles, fn_value_depth,
        typing_globals, display_params, dropped_closures, frozen_elems,
        literal_hosts, literal_display,
        iface_generators, copy_diags, copy_uses, unwraps, reported_captures, reported_mutations,
        in_shared, crossing_callbacks, matching_context, checking, ret_checks, overridden,
        moved_consts, borrow_pass_errors, fresh_checks, param_field_writes,
        param_proxy_targets, deferred_ts_returns, rechecks, literal_types,
        literal_found, late_consts, group_defs, readonly_widening,
        // The app's alone: empty in std's unit.
        jsx_providers, jsx_refusals, poisoned, added_defs,
    ],
}

/// A field's shape, to tell it from a fresh context's ([`empty_fields_differing`]): how many
/// entries a collection holds, a flag's or a counter's value.
#[cfg(test)]
pub(crate) trait Fresh {
    fn shape(&self) -> String;
}

#[cfg(test)]
mod fresh {
    use std::collections::{BTreeMap, BTreeSet};

    use rustc_hash::{FxHashMap, FxHashSet};

    use super::Fresh;

    macro_rules! by_len {
        ($($t:ident<$($g:ident),*>),*) => {$(
            impl<$($g),*> Fresh for $t<$($g),*> {
                fn shape(&self) -> String {
                    format!("{} entries", self.len())
                }
            }
        )*};
    }
    by_len!(Vec<T>, BTreeMap<K, V>, BTreeSet<K>, FxHashMap<K, V>, FxHashSet<K>);

    macro_rules! by_value {
        ($($t:ty),*) => {$(
            impl Fresh for $t {
                fn shape(&self) -> String {
                    format!("{self}")
                }
            }
        )*};
    }
    by_value!(bool, u32, usize);

    impl<T> Fresh for Option<T> {
        fn shape(&self) -> String {
            format!("some: {}", self.is_some())
        }
    }

    impl<T: Fresh> Fresh for std::cell::RefCell<T> {
        fn shape(&self) -> String {
            self.borrow().shape()
        }
    }
}

/// The fields saved in part.
mod special {
    use super::{Codec, Reader, Writer};
    use crate::type_level;
    use crate::type_queries::SigQueries;

    pub(super) trait Part {
        fn enc_part(&self, w: &mut Writer);
        fn dec_part(&mut self, r: &mut Reader);
    }

    pub(super) fn enc(_name: &str, v: &impl Part, w: &mut Writer) {
        v.enc_part(w);
    }

    pub(super) fn dec(_name: &str, v: &mut impl Part, r: &mut Reader) {
        v.dec_part(r);
    }

    /// The deferred signatures are filled and the flags reset once collection is done: only
    /// the read-only parameters stay.
    impl Part for SigQueries<'_> {
        fn enc_part(&self, w: &mut Writer) {
            let SigQueries {
                sig_fill: _,
                sig_deferred: _,
                deferred_sigs: _,
                filling_sigs: _,
                readonly_params,
                erase_generics: _,
                in_field: _,
                field_erased: _,
                param_props: _,
            } = self;
            readonly_params.enc(w);
        }
        fn dec_part(&mut self, r: &mut Reader) {
            self.readonly_params = Codec::dec(r);
        }
    }

    /// The deferred types and what is known of them, and the C pointees refused (so a later
    /// unit neither reports one again nor refuses a C signature for it); not the state of an
    /// expansion in progress, nor the memo of property-name kinds.
    impl Part for type_level::State {
        fn enc_part(&self, w: &mut Writer) {
            let type_level::State {
                alias_site: _,
                ptr_site: _,
                ptr_refused,
                ptr_refusals,
                ptr_refused_types,
                use_site: _,
                key_kinds: _,
                deferring: _,
                stuck: _,
                callable_params: _,
                op_sites,
                failed_sites,
                op_modules,
                intersection_parts,
                converts,
                spread_into,
                reduced,
                inline,
                alias_canon,
                alias_shapes,
            } = self;
            op_sites.enc(w);
            failed_sites.enc(w);
            op_modules.enc(w);
            intersection_parts.enc(w);
            converts.enc(w);
            spread_into.enc(w);
            reduced.enc(w);
            inline.enc(w);
            alias_canon.enc(w);
            alias_shapes.enc(w);
            ptr_refused.enc(w);
            ptr_refusals.enc(w);
            ptr_refused_types.enc(w);
        }
        fn dec_part(&mut self, r: &mut Reader) {
            self.op_sites = Codec::dec(r);
            self.failed_sites = Codec::dec(r);
            self.op_modules = Codec::dec(r);
            self.intersection_parts = Codec::dec(r);
            self.converts = Codec::dec(r);
            self.spread_into = Codec::dec(r);
            self.reduced = Codec::dec(r);
            self.inline = Codec::dec(r);
            self.alias_canon = Codec::dec(r);
            self.alias_shapes = Codec::dec(r);
            self.ptr_refused = Codec::dec(r);
            self.ptr_refusals = Codec::dec(r);
            self.ptr_refused_types = Codec::dec(r);
        }
    }
}

/// `cx`, a finished unit, encoded.
pub(crate) fn encode_unit(cx: &Ctx) -> Vec<u8> {
    encode_unit_sized(cx).0
}

/// [`encode_unit`], with the bytes each saved field took (and last, how many of them are AST
/// fragments).
pub(crate) fn encode_unit_sized(cx: &Ctx) -> (Vec<u8>, Vec<(&'static str, usize)>) {
    // No function keeps its body in the unit.
    super::ast_refs::keep_bodies(Default::default());
    let mut w = Writer::new();
    let mut sizes = vec![];
    // The modules' files first, in order: a decoder maps them to the modules it is given.
    for m in &cx.modules {
        w.file(m.file);
    }
    enc_fields(cx, &mut w, &mut sizes);
    sizes.push(("(of which AST)", w.ast_bytes));
    let (body, files) = w.finish();
    let mut head = Writer::new();
    head.out.extend_from_slice(FORMAT);
    head.uint(cx.modules.len() as u64);
    head.uint(files.len() as u64);
    // A file that is no module's (none so far) keeps its id, which only this session knows.
    for &f in &files[cx.modules.len().min(files.len())..] {
        head.uint(u64::from(f.0));
    }
    // The modules' paths, in order: a decoder puts the modules it is given in this order
    // ([`unit_module_paths`]).
    for m in &cx.modules {
        m.path.enc(&mut head);
    }
    let mut rest = head.out.split_off(FORMAT.len());
    rest.extend_from_slice(&body);
    let mut out = head.out;
    out.extend_from_slice(&checksum(&rest).to_le_bytes());
    out.extend_from_slice(&rest);
    (out, sizes)
}

/// The checksum of a unit's bytes after its [`FORMAT`] and the checksum itself: a damaged unit
/// (a cut file, a flipped bit) is refused before anything of it is decoded, and so is a cache
/// miss. Not a defence against a unit made to collide: the store is the user's own.
fn checksum(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write(bytes);
    h.write_usize(bytes.len());
    h.finish()
}

/// The unit [`encode_unit`] encoded, over `modules` (the unit's modules, in the order it had
/// them, with this session's files), its AST nodes decoded into `arena`; `None` when `bytes` is
/// not a whole encoding of this [`FORMAT`] or not of as many modules. The fields a unit does not
/// save are as [`Ctx::over`] makes them, and the definitions are frozen.
///
/// Runs on a thread with sema's stack (`SEMA_STACK_BYTES`): decoding nests as deep as the
/// unit's expressions, the fragments are decoded right there (`velt_syntax::with_stack_of`),
/// and the unit's decode stack is sema's (`with_decode_stack`). A unit whose checksum does not
/// match is `None`.
///
/// # Safety
///
/// `arena` must outlive the context returned and every copy of it ([`Arena`]).
pub(crate) unsafe fn decode_unit<'m>(
    bytes: &std::rc::Rc<Vec<u8>>,
    arena: &Arena,
    modules: Vec<&'m SourceModule>,
) -> Option<Ctx<'m>> {
    let sealed = bytes.strip_prefix(FORMAT.as_slice())?;
    let (sum, rest) = sealed.split_at_checked(8)?;
    if u64::from_le_bytes(sum.try_into().ok()?) != checksum(rest) {
        return None;
    }
    let mut head = Reader::bare(rest, &[]);
    let n = head.len();
    let nfiles = head.len();
    if n != modules.len() || nfiles < n {
        return None;
    }
    let mut files: Vec<FileId> = modules.iter().map(|m| m.file).collect();
    for _ in n..nfiles {
        files.push(FileId(head.u32()));
    }
    for m in &modules {
        if String::dec(&mut head) != m.path {
            return None;
        }
    }
    if head.failed {
        return None;
    }
    let body_at = FORMAT.len() + 8 + head.pos;
    let files = std::rc::Rc::new(files);
    let mut cx = Ctx::over(modules, 0);
    // On sema's stack, as the caller's thread is: the unit's readers, and with them every entry
    // of its tables decoded later, stop at that stack's depth (`Src` keeps it).
    let done = velt_syntax::with_stack_of(crate::SEMA_STACK_BYTES, || {
        super::with_decode_stack(crate::SEMA_STACK_BUDGET, || {
            // SAFETY: the caller keeps `arena` alive longer than `cx` (above).
            let mut r = unsafe { Reader::over_unit(bytes, body_at, &files, arena) };
            for i in 0..n {
                if r.uint() != i as u64 {
                    return false;
                }
            }
            dec_fields(&mut cx, &mut r);
            r.done()
        })
    });
    if !done {
        return None;
    }
    cx.freeze_defs();
    Some(cx)
}

/// The type table as its kinds in order: decoding interns them again, which gives each its id
/// back (a damaged table that does not is refused) and rebuilds the intern map.
impl Codec for Types {
    fn enc(&self, w: &mut Writer) {
        let Types {
            table,
            unit,
            never,
            error,
            bool_,
            str_,
            i32,
            i64,
            u64,
            usize,
            f64,
            bigint_named,
            inferred_fns,
            // The inverse of `readonly_elems`: built again when decoding.
            readonly_arrays: _,
            readonly_elems,
        } = self;
        // Before the table: a read-only array is a twin of `T[]` (`TyTable::twin`), which
        // interning its kind again would not give back.
        readonly_elems.enc(w);
        w.uint(table.len() as u64);
        for i in 0..table.len() as u32 {
            table.kind(TyId(i)).enc(w);
        }
        for t in [unit, never, error, bool_, str_, i32, i64, u64, usize, f64] {
            t.enc(w);
        }
        bigint_named.enc(w);
        inferred_fns.enc(w);
    }

    fn dec(r: &mut Reader) -> Self {
        let readonly_elems: rustc_hash::FxHashMap<TyId, TyId> = Codec::dec(r);
        let readonly_arrays = readonly_elems.iter().map(|(&a, &e)| (e, a)).collect();
        let n = r.len();
        let mut table = TyTable::new();
        for i in 0..n as u32 {
            let k = Codec::dec(r);
            if r.failed {
                break;
            }
            let id = match readonly_elems.contains_key(&TyId(i)) {
                true => table.twin(k),
                false => table.intern(k),
            };
            if id != TyId(i) {
                r.fail();
                break;
            }
        }
        let mut ids = [TyId(0); 10];
        for t in &mut ids {
            *t = TyId::dec(r);
        }
        let [unit, never, error, bool_, str_, i32, i64, u64, usize, f64] = ids;
        Types {
            table,
            unit,
            never,
            error,
            bool_,
            str_,
            i32,
            i64,
            u64,
            usize,
            f64,
            bigint_named: Codec::dec(r),
            inferred_fns: Codec::dec(r),
            readonly_arrays,
            readonly_elems,
        }
    }
}
super::structs! {
    Diagnostic { severity, message, labels, notes, code, fixes }
    Label { span, message }
    Fix { title, edits, applicability }
}

super::enums! {
    Severity {
        0 => Error,
        1 => Warning,
        2 => Note,
    }
    Applicability {
        0 => Safe,
        1 => Review,
    }
}

use velt_common::Applicability;

impl Codec for Box<str> {
    fn enc(&self, w: &mut Writer) {
        String::from(&**self).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        String::dec(r).into_boxed_str()
    }
}

/// The paths of the modules of the unit [`encode_unit`] encoded, in its order (`None` when
/// `bytes` is not of this [`FORMAT`]): [`decode_unit`] takes its modules in this order.
pub(crate) fn unit_module_paths(bytes: &[u8]) -> Option<Vec<String>> {
    let rest = bytes.strip_prefix(FORMAT.as_slice())?.get(8..)?;
    let mut head = Reader::bare(rest, &[]);
    let n = head.len();
    let nfiles = head.len();
    for _ in n..nfiles {
        head.u32();
    }
    let paths = (0..n).map(|_| String::dec(&mut head)).collect();
    (!head.failed).then_some(paths)
}
