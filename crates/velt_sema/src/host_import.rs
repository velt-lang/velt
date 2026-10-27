//! The link symbol of a host import (`declare module "wasm:<module>" { function name(…); }`,
//! docs/internals/design/cloudflare-workers.md §1). A host import is an extern function whose
//! symbol says where the wasm host provides it; codegen declares it as the wasm import
//! `(import "<module>" "<name>")`. The `:` can't appear in a Velt name, so no other symbol has
//! this form.

const PREFIX: &str = "velt_host:";

/// The symbol of host import `name` from import module `module`.
pub fn symbol(module: &str, name: &str) -> String {
    format!("{PREFIX}{module}:{name}")
}

/// The import module and name of a host import's symbol (`None`: any other symbol).
pub fn parse(symbol: &str) -> Option<(&str, &str)> {
    symbol.strip_prefix(PREFIX)?.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = symbol("velt_cf", "r2_get");
        assert_eq!(s, "velt_host:velt_cf:r2_get");
        assert_eq!(parse(&s), Some(("velt_cf", "r2_get")));
        assert_eq!(parse("velt_rt_panic"), None);
        assert_eq!(parse("r2_get"), None);
    }
}
