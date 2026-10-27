//! Host imports (`declare module "wasm:<name>"`, docs/internals/design/cloudflare-workers.md §1)
//! in a build: allowed only for a host that provides them.

/// Host imports (`declare module "wasm:<name>"`) link only in a build for a host that provides
/// them (`--host workers`); elsewhere the first one the program calls is an error.
pub(super) fn check(
    program: &velt_vir::vir::Program,
    host: Option<&str>,
) -> Result<(), super::BuildError> {
    if host.is_some() {
        return Ok(());
    }
    let first = program
        .externs
        .iter()
        .find_map(|e| velt_vir::host_import::parse(&e.symbol));
    match first {
        Some((module, name)) => Err(super::BuildError::Failed(format!(
            "`{name}` is a host import (`declare module \"wasm:{module}\"`): only a build for a host that provides it can call it (`--host workers`)"
        ))),
        None => Ok(()),
    }
}
