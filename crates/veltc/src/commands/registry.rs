//! `velt registry serve`: share a registry directory over HTTP (crate `velt_registry`).

use std::net::TcpListener;

use crate::cli::RegistryArgs;

/// Serve until interrupted. Uploads need `$VELT_REGISTRY_TOKEN` when it is set.
pub fn serve_command(args: &RegistryArgs) -> Result<(), String> {
    let root = match &args.dir {
        Some(d) => d.clone(),
        None => vpm::Locations::from_env()?.registry,
    };
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("cannot create `{}`: {e}", root.display()))?;
    let token = std::env::var(vpm::remote::TOKEN_VAR)
        .ok()
        .filter(|t| !t.is_empty());
    let listener = TcpListener::bind(&args.addr)
        .map_err(|e| format!("cannot listen on {}: {e}", args.addr))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let uploads = if token.is_some() {
        "uploads need the token"
    } else {
        "uploads are open: set $VELT_REGISTRY_TOKEN to require a token"
    };
    eprintln!(
        "velt registry: serving {} at http://{addr} ({uploads})",
        root.display()
    );
    let registry = velt_registry::Registry { root, token };
    velt_http::serve(&listener, registry.handler(), velt_registry::MAX_ARCHIVE);
    Ok(())
}
