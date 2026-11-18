//! A tiny size-aware LRU cache: a hash map plus a recency index, evicting the
//! least recently used entries once the byte budget is exceeded. Used for the
//! decoded-pixmap cache, the raw asset bytes and the tile cache.

use std::collections::{BTreeMap, HashMap};

struct Entry<V> {
    value: V,
    size: usize,
    tick: u64,
}

pub struct Lru<V> {
    budget: usize,
    used: usize,
    tick: u64,
    map: HashMap<String, Entry<V>>,
    /// tick -> key, oldest first.
    order: BTreeMap<u64, String>,
}

impl<V> Lru<V> {
    pub fn new(budget: usize) -> Self {
        Lru {
            budget,
            used: 0,
            tick: 0,
            map: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[allow(dead_code)]
    pub fn used(&self) -> usize {
        self.used
    }

    /// Look up `key` and mark it most recently used.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        let e = self.map.get_mut(key)?;
        self.order.remove(&e.tick);
        self.tick += 1;
        e.tick = self.tick;
        self.order.insert(self.tick, key.to_string());
        Some(&e.value)
    }

    /// Insert (or replace) `key` taking `size` bytes of the budget, evicting
    /// the least recently used entries until it fits. An entry larger than the
    /// whole budget is not cached.
    pub fn insert(&mut self, key: String, value: V, size: usize) {
        self.remove(&key);
        if size > self.budget {
            return;
        }
        while self.used + size > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.map.remove(&oldest) {
                self.used -= e.size;
            }
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.used += size;
        self.map.insert(
            key,
            Entry {
                value,
                size,
                tick: self.tick,
            },
        );
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(e) = self.map.remove(key) {
            self.order.remove(&e.tick);
            self.used -= e.size;
        }
    }

    /// Keep only the entries whose key satisfies `keep`.
    pub fn retain_keys(&mut self, keep: impl Fn(&str) -> bool) {
        let drop: Vec<String> = self.map.keys().filter(|k| !keep(k)).cloned().collect();
        for k in drop {
            self.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_the_least_recently_used_over_budget() {
        // 30 entries of 10 bytes into a budget that fits 25.
        let mut c: Lru<u32> = Lru::new(250);
        for i in 0..30u32 {
            c.insert(format!("k{i}"), i, 10);
        }
        assert_eq!(c.len(), 25);
        assert_eq!(c.used(), 250);
        for i in 0..5 {
            assert!(c.get(&format!("k{i}")).is_none(), "k{i} is evicted");
        }
        for i in 5..30 {
            assert_eq!(c.get(&format!("k{i}")), Some(&i), "k{i} is cached");
        }
    }

    #[test]
    fn a_hit_refreshes_recency() {
        let mut c: Lru<u32> = Lru::new(30);
        for i in 0..3u32 {
            c.insert(format!("k{i}"), i, 10);
        }
        assert!(c.get("k0").is_some()); // k0 is now the newest; k1 the oldest
        c.insert("k3".into(), 3, 10);
        assert!(c.get("k1").is_none());
        assert!(c.get("k0").is_some() && c.get("k2").is_some() && c.get("k3").is_some());
    }

    #[test]
    fn replace_and_oversize_keep_the_accounting_right() {
        let mut c: Lru<u32> = Lru::new(100);
        c.insert("a".into(), 1, 60);
        c.insert("a".into(), 2, 40);
        assert_eq!((c.used(), c.len()), (40, 1));
        c.insert("big".into(), 9, 101); // larger than the budget: not cached
        assert!(c.get("big").is_none());
        assert_eq!(c.get("a"), Some(&2));
    }
}
