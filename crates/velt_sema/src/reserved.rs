//! Property names Velt reserves for private names and symbol keys.

use crate::property_order;

/// Is `name` a property name a quoted key may not use: Velt's encodings of ES private names
/// (`#x`) and symbol keys (`[Symbol.iterator]`, and names starting with the noncharacter U+FFFF:
/// `property_order::SYMBOL_KEY_PREFIX`), or `__proto__`? Other names (`"[x]"`, `"[Symbol(k)]"`)
/// are ordinary keys: a computed key `[KEY]` is told apart by its span
/// (`symbols::is_computed_key`).
pub(crate) fn reserved_key(name: &str) -> bool {
    name.starts_with('#')
        || name.starts_with("[Symbol.")
        || name.starts_with(property_order::SYMBOL_KEY_PREFIX)
        || name == "__proto__"
}

/// Why property name `name` (a [`reserved_key`]) is not supported.
pub(crate) fn reserved_key_message(name: &str) -> String {
    if name == "__proto__" {
        return "the property name \"__proto__\" is not supported: in JavaScript it sets the object's prototype and creates no property".to_string();
    }
    format!(
        "the property name {name:?} is not supported: Velt uses names starting with {} for private names and symbol keys",
        reserved_prefixes(name)
    )
}

/// The reserved prefixes a message about property name `name` lists: U+FFFF too when `name`
/// starts with it (the key names of symbol constants).
pub(crate) fn reserved_prefixes(name: &str) -> &'static str {
    match name.starts_with(property_order::SYMBOL_KEY_PREFIX) {
        true => "`#`, `[Symbol.` and U+FFFF",
        false => "`#` and `[Symbol.`",
    }
}
