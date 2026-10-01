//! Backend registry / composition root (Step 2 contract).
//!
//! Owns backend instances and resolves `ObjectRef` to a backend. Backend
//! construction happens here, not inside command helpers (target.md §2).
//!
//! Retained storage contract surface; unused future operations remain explicit diagnostics.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::storage::error::StorageError;
use crate::storage::reference::{BackendId, ObjectRef};
use crate::storage::traits::StorageBackend;

/// Composition root: maps `BackendId` to a backend instance.
pub(crate) struct BackendRegistry {
    backends: BTreeMap<String, Arc<dyn StorageBackend>>,
}

impl BackendRegistry {
    pub(crate) fn new() -> Self {
        Self {
            backends: BTreeMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        backend: Arc<dyn StorageBackend>,
    ) -> Result<(), StorageError> {
        let id = backend.id();
        if self.backends.contains_key(id.as_str()) {
            return Err(StorageError::AlreadyExists {
                path: format!("backend {}", id.as_str()),
            });
        }
        self.backends.insert(id.as_str().to_string(), backend);
        Ok(())
    }

    pub(crate) fn get(&self, id: &BackendId) -> Option<&Arc<dyn StorageBackend>> {
        self.backends.get(id.as_str())
    }

    pub(crate) fn resolve(
        &self,
        reference: &ObjectRef,
    ) -> Result<&Arc<dyn StorageBackend>, StorageError> {
        self.get(reference.backend())
            .ok_or_else(|| StorageError::NotFound {
                path: format!("backend {}", reference.backend().as_str()),
            })
    }

    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<BackendId> {
        self.backends
            .keys()
            .filter_map(|k| BackendId::new(k.clone()).ok())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::reference::ObjectKey;

    #[test]
    fn empty_registry_resolves_nothing() {
        let reg = BackendRegistry::new();
        assert!(reg.is_empty());
        let reference = ObjectRef::new(
            BackendId::new("mem").unwrap(),
            ObjectKey::new("a/b").unwrap(),
        );
        assert!(reg.resolve(&reference).is_err());
        assert!(reg.get(&BackendId::new("mem").unwrap()).is_none());
    }

    #[test]
    fn default_is_empty() {
        let reg = BackendRegistry::default();
        assert!(reg.is_empty());
        assert!(reg.ids().is_empty());
    }
}
