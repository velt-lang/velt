//! The per-user directory defaults to `~/.velt` (named after the tool, like `~/.cargo`), and
//! `$VELT_HOME` / `$VELT_REGISTRY` override it. Environment variables are process-wide, so this
//! binary holds a single test.

use vpm::Locations;

#[test]
fn home_defaults_to_dot_velt_and_env_overrides_it() {
    let tmp = tempfile::tempdir().unwrap();
    let user = tmp.path().join("user");
    std::env::remove_var("VELT_HOME");
    std::env::remove_var("VELT_REGISTRY");
    std::env::set_var("HOME", &user);
    std::env::set_var("USERPROFILE", &user);

    let loc = Locations::from_env().unwrap();
    assert_eq!(loc.registry, user.join(".velt").join("registry"));
    assert_eq!(loc.cache, user.join(".velt").join("cache"));
    // Nothing set: the public registry.
    assert_eq!(
        loc.remote.as_deref(),
        Some(vpm::locations::DEFAULT_REGISTRY)
    );

    let home = tmp.path().join("custom");
    let registry = tmp.path().join("shared-registry");
    std::env::set_var("VELT_HOME", &home);
    std::env::set_var("VELT_REGISTRY", &registry);
    let loc = Locations::from_env().unwrap();
    assert_eq!(loc.cache, home.join("cache"));
    assert_eq!(loc.registry, registry);
    assert_eq!(loc.remote, None, "a directory is a local registry");

    std::env::set_var("VELT_REGISTRY", "https://registry-staging.veltpm.com");
    let loc = Locations::from_env().unwrap();
    assert_eq!(
        loc.remote.as_deref(),
        Some("https://registry-staging.veltpm.com")
    );
}
