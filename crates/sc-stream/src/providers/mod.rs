//! The stream providers that are compiled in (TODO §11, task 1.4).
//!
//! One of them, when it lands: **MQTT** (Phase 4), behind a default-on feature
//! so a build can drop it — `smartcore`'s arrangement, for `smartcore`'s
//! reason. Everything else a stream can be observed from comes from a module
//! (§12).
//!
//! An **empty** built-in set is therefore a real state and not a bug: a build
//! with `--no-default-features` and no module installed has no stream
//! providers, and the Streams screen says so in a sentence rather than showing
//! an empty picker (task 6.1).

use std::sync::Arc;

use sc_error::Result;

use crate::provider::StreamProvider;
use crate::registry::StreamRegistry;

/// Every compiled-in stream provider.
///
/// Empty until Phase 4 adds MQTT: the trait, the registry and the supervisor
/// are what the built-in has to fit into, and writing them against a scripted
/// provider first is what keeps the seam honest (§13).
pub fn builtin_providers() -> Vec<Arc<dyn StreamProvider>> {
    Vec::new()
}

/// A registry holding the built-in providers.
///
/// Fallible because [`StreamRegistry::register`] is: two built-ins under one
/// name is a programming error, and it is better found at boot with both names
/// in the message than by whichever one happened to win.
pub fn builtin_registry() -> Result<StreamRegistry> {
    let mut registry = StreamRegistry::new();
    for provider in builtin_providers() {
        registry.register(provider)?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_set_assembles_into_a_registry() {
        let registry = builtin_registry().unwrap();
        assert_eq!(registry.len(), builtin_providers().len());
    }
}
