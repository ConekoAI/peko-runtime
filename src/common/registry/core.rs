//! Core registry types
//!
//! [`SimpleRegistry`] — in-memory `HashMap` wrapper for single-owner scenarios.
//! [`SharedRegistry`] — `Arc<RwLock<HashMap>>` wrapper for shared scenarios.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory registry with no locking overhead.
///
/// Use this when the registry is owned by a single struct and all mutations
/// happen through `&mut self`. This is the zero-cost option — it is just a
/// `HashMap` with a consistent registry-shaped API.
///
/// # Example
/// ```rust,ignore
/// let mut registry = SimpleRegistry::new();
/// registry.insert("foo".to_string(), 42);
/// assert_eq!(registry.get(&"foo".to_string()), Some(&42));
/// ```
#[derive(Debug, Clone)]
pub struct SimpleRegistry<K, V> {
    inner: HashMap<K, V>,
}

impl<K, V> Default for SimpleRegistry<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> SimpleRegistry<K, V> {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Create a new registry with the given capacity.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: HashMap::with_capacity(cap),
        }
    }

    /// Insert a key-value pair.
    ///
    /// Returns the old value if the key already existed.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Eq + Hash,
    {
        self.inner.insert(key, value)
    }

    /// Get a reference to a value by key.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V>
    where
        K: Eq + Hash,
    {
        self.inner.get(key)
    }

    /// Get a mutable reference to a value by key.
    #[must_use]
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V>
    where
        K: Eq + Hash,
    {
        self.inner.get_mut(key)
    }

    /// Remove a key and return its value.
    pub fn remove(&mut self, key: &K) -> Option<V>
    where
        K: Eq + Hash,
    {
        self.inner.remove(key)
    }

    /// Check if a key exists.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool
    where
        K: Eq + Hash,
    {
        self.inner.contains_key(key)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.inner.iter()
    }

    /// Iterate over values.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.inner.values()
    }

    /// Iterate over keys.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.inner.keys()
    }

    /// Get a mutable entry for the given key.
    pub fn entry(&mut self, key: K) -> std::collections::hash_map::Entry<'_, K, V>
    where
        K: Eq + Hash,
    {
        self.inner.entry(key)
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Retain only entries matching the predicate.
    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        self.inner.retain(f);
    }

    /// Drain all entries into an iterator.
    pub fn drain(&mut self) -> std::collections::hash_map::Drain<'_, K, V> {
        self.inner.drain()
    }

    /// Expose the underlying `HashMap` for advanced operations.
    #[must_use]
    pub fn inner(&self) -> &HashMap<K, V> {
        &self.inner
    }

    /// Expose the underlying `HashMap` mutably for advanced operations.
    pub fn inner_mut(&mut self) -> &mut HashMap<K, V> {
        &mut self.inner
    }
}

// ============================================================================
// SharedRegistry
// ============================================================================

/// Thread-safe, shared registry.
///
/// Replaces the hand-rolled `Arc<RwLock<HashMap<K, V>>>` pattern that is
/// duplicated across the codebase. Provides async CRUD methods and batch
/// read/write helpers.
///
/// The internal implementation can later be swapped for `dashmap` or `scc`
/// without changing the public API.
///
/// # Example
/// ```rust,ignore
/// let registry = SharedRegistry::new();
/// registry.insert("foo".to_string(), 42).await;
/// assert_eq!(registry.get(&"foo".to_string()).await, Some(42));
/// ```
#[derive(Debug)]
pub struct SharedRegistry<K, V> {
    inner: Arc<RwLock<HashMap<K, V>>>,
}

impl<K, V> Clone for SharedRegistry<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> Default for SharedRegistry<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> SharedRegistry<K, V> {
    /// Create a new empty shared registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new shared registry with the given capacity.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::with_capacity(cap))),
        }
    }
}

impl<K, V> SharedRegistry<K, V>
where
    K: Eq + Hash + Send + Sync,
    V: Send + Sync + Clone,
{
    /// Insert a key-value pair.
    pub async fn insert(&self, key: K, value: V) {
        let mut guard = self.inner.write().await;
        guard.insert(key, value);
    }

    /// Get a cloned value by key.
    #[must_use]
    pub async fn get(&self, key: &K) -> Option<V> {
        let guard = self.inner.read().await;
        guard.get(key).cloned()
    }

    /// Remove a key and return its cloned value.
    pub async fn remove(&self, key: &K) -> Option<V> {
        let mut guard = self.inner.write().await;
        guard.remove(key)
    }

    /// Check if a key exists.
    #[must_use]
    pub async fn contains(&self, key: &K) -> bool {
        let guard = self.inner.read().await;
        guard.contains_key(key)
    }

    /// Number of entries.
    #[must_use]
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Check if empty.
    #[must_use]
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }

    /// Return a vector of all cloned values.
    #[must_use]
    pub async fn values(&self) -> Vec<V> {
        let guard = self.inner.read().await;
        guard.values().cloned().collect()
    }

    /// Return a vector of all cloned keys.
    #[must_use]
    pub async fn keys(&self) -> Vec<K>
    where
        K: Clone,
    {
        let guard = self.inner.read().await;
        guard.keys().cloned().collect()
    }

    /// Clear all entries.
    pub async fn clear(&self) {
        let mut guard = self.inner.write().await;
        guard.clear();
    }

    /// Batch read — acquires the read lock once for multiple operations.
    ///
    /// Prefer this over multiple individual `get` calls when you need
    /// consistency across multiple lookups.
    pub async fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&HashMap<K, V>) -> R,
    {
        let guard = self.inner.read().await;
        f(&guard)
    }

    /// Batch write — acquires the write lock once for multiple operations.
    pub async fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut HashMap<K, V>) -> R,
    {
        let mut guard = self.inner.write().await;
        f(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_registry_basic() {
        let mut reg = SimpleRegistry::new();
        assert!(reg.is_empty());

        reg.insert("a", 1);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get(&"a"), Some(&1));
        assert!(reg.contains(&"a"));

        let old = reg.insert("a", 2);
        assert_eq!(old, Some(1));
        assert_eq!(reg.get(&"a"), Some(&2));

        let removed = reg.remove(&"a");
        assert_eq!(removed, Some(2));
        assert!(reg.is_empty());
    }

    #[test]
    fn test_simple_registry_retain() {
        let mut reg = SimpleRegistry::new();
        reg.insert("a", 1);
        reg.insert("b", 2);
        reg.insert("c", 3);

        reg.retain(|_, v| *v > 1);
        assert_eq!(reg.len(), 2);
        assert!(!reg.contains(&"a"));
        assert!(reg.contains(&"b"));
        assert!(reg.contains(&"c"));
    }

    #[tokio::test]
    async fn test_shared_registry_basic() {
        let reg = SharedRegistry::new();
        assert!(reg.is_empty().await);

        reg.insert("a", 1).await;
        assert_eq!(reg.len().await, 1);
        assert_eq!(reg.get(&"a").await, Some(1));
        assert!(reg.contains(&"a").await);

        let removed = reg.remove(&"a").await;
        assert_eq!(removed, Some(1));
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn test_shared_registry_batch_read() {
        let reg = SharedRegistry::new();
        reg.insert("a", 1).await;
        reg.insert("b", 2).await;

        let (a, b, len) = reg
            .read(|map| (map.get("a").copied(), map.get("b").copied(), map.len()))
            .await;

        assert_eq!(a, Some(1));
        assert_eq!(b, Some(2));
        assert_eq!(len, 2);
    }

    #[tokio::test]
    async fn test_shared_registry_batch_write() {
        let reg = SharedRegistry::new();
        reg.insert("a", 1).await;

        reg.write(|map| {
            map.insert("b", 2);
            map.insert("c", 3);
        })
        .await;

        assert_eq!(reg.len().await, 3);
    }

    #[tokio::test]
    async fn test_shared_registry_clone() {
        let reg1 = SharedRegistry::new();
        reg1.insert("a", 1).await;

        let reg2 = reg1.clone();
        assert_eq!(reg2.get(&"a").await, Some(1));

        reg2.insert("b", 2).await;
        assert_eq!(reg1.len().await, 2); // shared state
    }
}
