//! Scheme -> provider lookup.
//!
//! Mirrors `atomartist-lib`'s node `registry.rs`: shells register the
//! providers their build supports at startup (`demo-native` registers the
//! local filesystem, `demo-wasm` the browser store, a MatterHackers build adds
//! its cloud provider), `AppState` holds an `Arc<StorageRegistry>`, and every
//! operation resolves through [`StorageRegistry::resolve`].
//!
//! Registration order is preserved because it is the order the provider
//! sidebar lists in.

use std::collections::HashMap;
use std::sync::Arc;

use crate::provider::StorageProvider;
use crate::uri::StorageUri;

/// Registering a second provider for a scheme that is already taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateScheme(pub String);

impl std::fmt::Display for DuplicateScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "storage scheme `{}` is already registered", self.0)
    }
}

impl std::error::Error for DuplicateScheme {}

#[derive(Default)]
pub struct StorageRegistry {
    by_scheme: HashMap<String, Arc<dyn StorageProvider>>,
    /// Registration order of schemes, so listing is deterministic.
    order: Vec<String>,
}

impl StorageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a provider. Errors instead of panicking so a shell that loads
    /// provider plug-ins from configuration can report the clash.
    pub fn register(&mut self, provider: Arc<dyn StorageProvider>) -> Result<(), DuplicateScheme> {
        let scheme = provider.scheme().to_ascii_lowercase();
        if self.by_scheme.contains_key(&scheme) {
            return Err(DuplicateScheme(scheme));
        }
        self.order.push(scheme.clone());
        self.by_scheme.insert(scheme, provider);
        Ok(())
    }

    /// Provider that owns `uri`'s scheme.
    pub fn resolve(&self, uri: &StorageUri) -> Option<Arc<dyn StorageProvider>> {
        self.by_scheme(uri.scheme())
    }

    pub fn by_scheme(&self, scheme: &str) -> Option<Arc<dyn StorageProvider>> {
        self.by_scheme
            .get(&scheme.to_ascii_lowercase())
            .map(Arc::clone)
    }

    /// Providers in registration order.
    pub fn providers(&self) -> impl Iterator<Item = &Arc<dyn StorageProvider>> {
        self.order
            .iter()
            .filter_map(move |scheme| self.by_scheme.get(scheme))
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryProvider;

    #[test]
    fn resolves_by_scheme_and_preserves_order() {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(MemoryProvider::new("mem", "Memory")))
            .unwrap();
        registry
            .register(Arc::new(MemoryProvider::new("other", "Other")))
            .unwrap();

        assert_eq!(registry.len(), 2);
        let schemes: Vec<_> = registry
            .providers()
            .map(|p| p.scheme().to_string())
            .collect();
        assert_eq!(schemes, vec!["mem", "other"]);

        let uri: StorageUri = "other:///a.atmr".parse().unwrap();
        let provider = registry.resolve(&uri).unwrap();
        assert_eq!(provider.display_name(), "Other");

        let unknown: StorageUri = "nope:///a.atmr".parse().unwrap();
        assert!(registry.resolve(&unknown).is_none());
    }

    #[test]
    fn duplicate_scheme_is_rejected() {
        let mut registry = StorageRegistry::new();
        registry
            .register(Arc::new(MemoryProvider::new("mem", "Memory")))
            .unwrap();
        let err = registry
            .register(Arc::new(MemoryProvider::new("mem", "Second")))
            .unwrap_err();
        assert_eq!(err, DuplicateScheme("mem".to_string()));
        assert_eq!(registry.len(), 1);
    }
}
