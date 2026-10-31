//! Bounded in-memory cache primitives for page ink layers and raster tiles.
//! Both the base and its tiles are bound to the same snapshot identity.
use rs_render::raster::InkTile;
use std::collections::VecDeque;
use std::sync::Arc;

/// Snapshot identity is part of the key: revisions alone can repeat across
/// notes, and a pending/synced transition can replace a snapshot in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InkLayerPageKey {
    pub note_id: String,
    pub revision: i64,
    pub page_index: i64,
    pub snapshot_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InkLayerCacheKey {
    Base(InkLayerPageKey),
    Svg(InkLayerPageKey),
    NoText(InkLayerPageKey),
    FullText(InkLayerPageKey),
    TextBase(InkLayerPageKey),
    TextSvg(InkLayerPageKey),
    TextSize(InkLayerPageKey),
    TextTile { page: InkLayerPageKey, tile: InkTile },
    Tile { page: InkLayerPageKey, tile: InkTile },
}

/// Least-recently-used cache of encoded bytes, with a bound on retained entries.
/// Returned Arc handles may outlive eviction. The caller serializes access and
/// chooses a budget shared with its previews.
pub struct InkLayerCache {
    entries: VecDeque<(InkLayerCacheKey, Arc<[u8]>)>,
    bytes: usize,
    budget: usize,
    max_entries: usize,
}

impl InkLayerCache {
    pub fn new(budget: usize, max_entries: usize) -> Self {
        Self { entries: VecDeque::new(), bytes: 0, budget, max_entries }
    }

    pub fn get(&mut self, key: &InkLayerCacheKey) -> Option<Arc<[u8]>> {
        let index = self.entries.iter().position(|(candidate, _)| candidate == key)?;
        let entry = self.entries.remove(index)?;
        let result = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(result)
    }

    /// Oversized entries are refused without evicting an existing value.
    pub fn insert(&mut self, key: InkLayerCacheKey, value: Arc<[u8]>) -> bool {
        if value.len() > self.budget || self.max_entries == 0 { return false; }
        if let Some(index) = self.entries.iter().position(|(candidate, _)| candidate == &key) {
            let (_, previous) = self.entries.remove(index).unwrap();
            self.bytes -= previous.len();
        }
        while self.entries.len() >= self.max_entries || value.len() > self.budget - self.bytes {
            let Some((_, previous)) = self.entries.pop_front() else { return false };
            self.bytes -= previous.len();
        }
        self.bytes += value.len();
        self.entries.push_back((key, value));
        true
    }

    pub fn bytes(&self) -> usize { self.bytes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_render::raster::InkTileScale;

    fn key(revision: i64, column: u32, digest: u8) -> InkLayerCacheKey {
        let tile = InkTile::new(1200, 1400, InkTileScale::from_half_units(2).unwrap(), column, 0).unwrap();
        InkLayerCacheKey::Tile { page: InkLayerPageKey { note_id: "synthetic".into(), revision, page_index: 0, snapshot_digest: [digest; 32] }, tile }
    }

    #[test]
    fn identity_includes_revision_snapshot_bucket_and_tile() {
        assert_ne!(key(1, 0, 0), key(2, 0, 0));
        assert_ne!(key(1, 0, 0), key(1, 1, 0));
        assert_ne!(key(1, 0, 0), key(1, 0, 1));
        let InkLayerCacheKey::Tile { page, .. } = key(1, 0, 0) else { unreachable!() };
        let other = InkLayerCacheKey::Tile { page: page.clone(), tile: InkTile::new(1200, 1400, InkTileScale::from_half_units(3).unwrap(), 0, 0).unwrap() };
        assert_ne!(key(1, 0, 0), other);
        assert_ne!(key(1, 0, 0), InkLayerCacheKey::Svg(page));
    }

    #[test]
    fn lru_bounds_replacement_and_oversized_values() {
        let mut cache = InkLayerCache::new(8, 2);
        let data = |n| Arc::<[u8]>::from(vec![0; n]);
        let a = key(1, 0, 0); let b = key(1, 1, 0); let c = key(1, 2, 0);
        assert!(cache.insert(a.clone(), data(4))); assert!(cache.insert(b.clone(), data(4)));
        assert!(!cache.insert(a.clone(), data(9))); assert_eq!(cache.bytes(), 8);
        assert!(cache.get(&a).is_some()); assert!(cache.insert(c.clone(), data(4)));
        assert!(cache.get(&b).is_none()); assert!(cache.get(&a).is_some());
        assert!(cache.insert(a.clone(), data(2))); assert_eq!(cache.bytes(), 6);
        assert!(cache.insert(b.clone(), data(5))); assert!(cache.get(&c).is_none());
        assert!(cache.get(&a).is_some()); assert_eq!(cache.bytes(), 7);
        assert!(!InkLayerCache::new(8, 0).insert(a, data(1)));
    }
}
