//! An immutable snapshot of all collections (the schema cache, SPEC §8.5).

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use super::collection::Collection;

/// All collections at one point in time. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    collections: Vec<Arc<Collection>>,
    by_name: HashMap<String, usize>,
    by_id: HashMap<Uuid, usize>,
    /// Incremented on every schema change; part of compiled-rule cache keys.
    pub version: u64,
}

impl Schema {
    /// Builds a snapshot.
    pub fn new(collections: Vec<Collection>, version: u64) -> Self {
        let collections: Vec<Arc<Collection>> = collections.into_iter().map(Arc::new).collect();
        let by_name = collections
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let by_id = collections
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        Self {
            collections,
            by_name,
            by_id,
            version,
        }
    }

    /// Collection by name.
    pub fn by_name(&self, name: &str) -> Option<&Arc<Collection>> {
        self.by_name.get(name).map(|&i| &self.collections[i])
    }

    /// Collection by id.
    pub fn by_id(&self, id: Uuid) -> Option<&Arc<Collection>> {
        self.by_id.get(&id).map(|&i| &self.collections[i])
    }

    /// Collection by id or name.
    pub fn by_id_or_name(&self, s: &str) -> Option<&Arc<Collection>> {
        Uuid::try_parse(s)
            .ok()
            .and_then(|id| self.by_id(id))
            .or_else(|| self.by_name(s))
    }

    /// All collections, in creation order.
    pub fn all(&self) -> &[Arc<Collection>] {
        &self.collections
    }

    /// A copy with `c` inserted or replaced (by id).
    pub fn with(&self, c: Collection) -> Self {
        let mut list: Vec<Collection> = self.collections.iter().map(|c| (**c).clone()).collect();
        match list.iter().position(|x| x.id == c.id) {
            Some(i) => list[i] = c,
            None => list.push(c),
        }
        Self::new(list, self.version + 1)
    }

    /// A copy without the collection `id`.
    pub fn without(&self, id: Uuid) -> Self {
        let list = self
            .collections
            .iter()
            .filter(|c| c.id != id)
            .map(|c| (**c).clone())
            .collect();
        Self::new(list, self.version + 1)
    }
}
