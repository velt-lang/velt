//! What an install warns of.

use crate::lockfile::Lockfile;
use crate::resolve::{Resolution, Source};

/// The resolution's warnings, and one per URL that holds what looks like a credential and that
/// `existing` doesn't lock yet: said once, when it is added, as every command resolves and the
/// URL may be signed on purpose.
pub(super) fn of(resolution: &Resolution, existing: Option<&Lockfile>) -> Vec<String> {
    let locked = |source: &str| {
        existing.is_some_and(|lock| lock.packages.iter().any(|p| p.source == source))
    };
    let credentials = resolution
        .packages
        .iter()
        .filter_map(|(name, p)| match &p.source {
            Source::Url { spec, .. } if !locked(&spec.source()) => {
                crate::url_source::credential_warning(name, &spec.url)
            }
            _ => None,
        });
    (resolution.warnings.iter().cloned())
        .chain(credentials)
        .collect()
}
