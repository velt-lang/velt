//! The key table of [`super`]: the symbol constants of std/lang/hooks/keys.velt that key the std
//! methods the compiler calls.

/// One key of a std method the compiler calls: a symbol constant of std/lang/hooks/keys.velt.
#[derive(Clone, Copy, Debug)]
pub struct Key {
    /// The entry's name (`map_cursor`).
    pub name: &'static str,
    /// The constant in std/lang/hooks/keys.velt (`MAP_CURSOR`).
    pub constant: &'static str,
    /// The symbol's description (`mapCursor`), or its registry key for a registered one.
    pub description: &'static str,
    /// `Symbol.for(key)`, which programs reach too (a protocol they implement: `inspect`), not
    /// `Symbol(description)`, which only std holds.
    pub registered: bool,
}

macro_rules! keys {
    (@registered) => { false };
    (@registered registered) => { true };
    ($($(#[doc = $doc:literal])* $name:ident => $constant:literal, $desc:literal $(, $reg:ident)?;)*) => {
        /// The member name of the std method keyed by entry `name` of the key table: what sema
        /// names a member `[KEY]` declared with that symbol (`"\u{FFFF}Symbol(mapCursor)"`;
        /// velt_sema's `property_order::symbol_key_name`, which names std's symbols first).
        #[macro_export]
        macro_rules! std_key {
            $( ($name) => { concat!("\u{FFFF}Symbol(", $desc, ")") }; )*
        }

        /// Every key, in table order.
        pub const KEYS: &[Key] = &[$(Key {
            name: stringify!($name),
            constant: $constant,
            description: $desc,
            registered: keys!(@registered $($reg)?),
        }),*];
    };
}

keys! {
    // What `console.log` and `util.inspect` call, as Node's `util.inspect.custom`: a program's
    // class implements it with `const custom = Symbol.for("nodejs.util.inspect.custom")` and a
    // method `[custom]()` (`velt_sema` `hooks.rs`).
    inspect => "INSPECT", "nodejs.util.inspect.custom", registered;
    // Typed arrays: the kind a class's elements are (a subclass may override it).
    typed_array_kind => "TYPED_ARRAY_KIND", "velt.typedArray.kind", registered;
    // Arrays: `xs[i]` that reads `undefined` past the end, as JS (`velt_sema` `indexers.rs`).
    array_get => "ARRAY_GET", "arrayGet";
    // `xs.reduce(f)` without an initial value (`velt_sema` `method_call.rs`).
    array_reduce_first => "ARRAY_REDUCE_FIRST", "arrayReduceFirst";
    array_reduce_right_first => "ARRAY_REDUCE_RIGHT_FIRST", "arrayReduceRightFirst";
    // JsonValue: what `console.log` prints a value from (`velt_vir` `glue/format.rs`), what
    // appends one's text (velt/encoding/json/append) and what decoders read one with
    // (velt/encoding/json/generate/read_value).
    json_runtime_handle => "JSON_RUNTIME_HANDLE", "jsonRuntimeHandle";
    json_write => "JSON_WRITE", "jsonWrite";
    json_from_reader => "JSON_FROM_READER", "jsonFromReader";
    // Map: what a `for...of` over a map reads (`velt_sema` `for_map.rs`, `velt_vir` `frozen.rs`),
    // what `console.log` calls (`velt_vir` `glue/format_map.rs`).
    map_cursor => "MAP_CURSOR", "mapCursor";
    map_advance => "MAP_ADVANCE", "mapAdvance";
    map_key_at => "MAP_KEY_AT", "mapKeyAt";
    map_value_at => "MAP_VALUE_AT", "mapValueAt";
    map_entry_at => "MAP_ENTRY_AT", "mapEntryAt";
    map_live_at => "MAP_LIVE_AT", "mapLiveAt";
    // ThisFn: a call with a `this` and `bind` (`velt_vir` `this_fn.rs`).
    this_fn_apply => "THIS_FN_APPLY", "thisFnApply";
    this_fn_bind => "THIS_FN_BIND", "thisFnBind";
    // Record: what the record syntax and `Object.*` compile to (`velt_sema` `record*.rs`,
    // `velt_vir` `any_record.rs`, `json/map.rs`).
    record_at => "RECORD_AT", "recordAt";
    record_decoded => "RECORD_DECODED", "recordDecoded";
    record_delete => "RECORD_DELETE", "recordDelete";
    record_entries => "RECORD_ENTRIES", "recordEntries";
    record_extend => "RECORD_EXTEND", "recordExtend";
    record_extend_from => "RECORD_EXTEND_FROM", "recordExtendFrom";
    record_extend_maybe => "RECORD_EXTEND_MAYBE", "recordExtendMaybe";
    record_get => "RECORD_GET", "recordGet";
    record_has => "RECORD_HAS", "recordHas";
    record_in_any => "RECORD_IN_ANY", "recordInAny";
    record_is_null_proto => "RECORD_IS_NULL_PROTO", "recordIsNullProto";
    record_key_names => "RECORD_KEY_NAMES", "recordKeyNames";
    record_mark_null_proto => "RECORD_MARK_NULL_PROTO", "recordMarkNullProto";
    record_perm => "RECORD_PERM", "recordPerm";
    record_positions => "RECORD_POSITIONS", "recordPositions";
    record_set => "RECORD_SET", "recordSet";
    record_size => "RECORD_SIZE", "recordSize";
    record_values => "RECORD_VALUES", "recordValues";
    record_accessor => "RECORD_ACCESSOR", "recordAccessor";
    record_define_accessor => "RECORD_DEFINE_ACCESSOR", "recordDefineAccessor";
    record_define_getter => "RECORD_DEFINE_GETTER", "recordDefineGetter";
    record_define_setter => "RECORD_DEFINE_SETTER", "recordDefineSetter";
    record_delete_keyed => "RECORD_DELETE_KEYED", "recordDeleteKeyed";
    record_delete_sym => "RECORD_DELETE_SYM", "recordDeleteSym";
    record_get_keyed => "RECORD_GET_KEYED", "recordGetKeyed";
    record_get_sym => "RECORD_GET_SYM", "recordGetSym";
    record_has_keyed => "RECORD_HAS_KEYED", "recordHasKeyed";
    record_has_sym => "RECORD_HAS_SYM", "recordHasSym";
    record_in_keyed => "RECORD_IN_KEYED", "recordInKeyed";
    record_own_keys => "RECORD_OWN_KEYS", "recordOwnKeys";
    record_own_symbols => "RECORD_OWN_SYMBOLS", "recordOwnSymbols";
    record_put_accessor => "RECORD_PUT_ACCESSOR", "recordPutAccessor";
    record_set_keyed => "RECORD_SET_KEYED", "recordSetKeyed";
    record_set_sym => "RECORD_SET_SYM", "recordSetSym";
    record_descriptor => "RECORD_DESCRIPTOR", "recordDescriptor";
    record_put_sym => "RECORD_PUT_SYM", "recordPutSym";
    // Proxy: what an operation on `any` asks a proxy (`velt_vir` `any_proxy.rs` & co.).
    proxy_get_getter => "PROXY_GET_GETTER", "proxyGetGetter";
    proxy_reads_getter => "PROXY_READS_GETTER", "proxyReadsGetter";
    proxy_view => "PROXY_VIEW", "proxyView";
    proxy_is_view => "PROXY_IS_VIEW", "proxyIsView";
    proxy_plain => "PROXY_PLAIN", "proxyPlain";
    proxy_has_get => "PROXY_HAS_GET", "proxyHasGet";
    proxy_is_array => "PROXY_IS_ARRAY", "proxyIsArray";
    proxy_self => "PROXY_SELF", "proxySelf";
    proxy_target => "PROXY_TARGET", "proxyTarget";
    proxy_get => "PROXY_GET", "proxyGet";
    proxy_set => "PROXY_SET", "proxySet";
    proxy_has => "PROXY_HAS", "proxyHas";
    proxy_delete_property => "PROXY_DELETE_PROPERTY", "proxyDeleteProperty";
    proxy_delete => "PROXY_DELETE", "proxyDelete";
    proxy_own_keys => "PROXY_OWN_KEYS", "proxyOwnKeys";
    proxy_get_own_property_descriptor => "PROXY_GET_OWN_PROPERTY_DESCRIPTOR", "proxyGetOwnPropertyDescriptor";
    proxy_define_property => "PROXY_DEFINE_PROPERTY", "proxyDefineProperty";
    proxy_keys => "PROXY_KEYS", "proxyKeys";
    proxy_defines => "PROXY_DEFINES", "proxyDefines";
    proxy_receive => "PROXY_RECEIVE", "proxyReceive";
    proxy_json => "PROXY_JSON", "proxyJson";
    proxy_array_call => "PROXY_ARRAY_CALL", "proxyArrayCall";
    proxy_has_own => "PROXY_HAS_OWN", "proxyHasOwn";
}
