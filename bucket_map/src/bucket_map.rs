//! BucketMap is a mostly contention free concurrent map backed by MmapMut

use crate::bucket::Bucket;
use crate::data_bucket::BucketMapStats;
use solana_sdk::pubkey::Pubkey;
use std::convert::TryInto;
use std::fmt::Debug;
use std::fs;
use std::ops::RangeBounds;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tempfile::TempDir;

pub type MaxSearch = u8;
pub type RefCount = u64;

pub struct BucketMapKeyValue<T> {
    pub pubkey: Pubkey,
    pub ref_count: RefCount,
    pub slot_list: Vec<T>,
}

#[derive(Debug, Default)]
pub struct BucketMapConfig {
    pub num_buckets: usize,
    pub drives: Option<Vec<PathBuf>>,
    pub max_search: Option<MaxSearch>,
}

impl BucketMapConfig {
    pub fn new(num_buckets: usize) -> BucketMapConfig {
        BucketMapConfig {
            num_buckets,
            ..BucketMapConfig::default()
        }
    }
}

pub struct BucketMap<T: Clone + Copy + Debug> {
    buckets: Vec<RwLock<Option<Bucket<T>>>>,
    drives: Arc<Vec<PathBuf>>,
    num_buckets_pow2: u8,
    max_search: MaxSearch,
    pub stats: Arc<BucketMapStats>,
    pub temp_dir: Option<TempDir>,
}

impl<T: Clone + Copy + Debug> Drop for BucketMap<T> {
    fn drop(&mut self) {
        if self.temp_dir.is_none() {
            BucketMap::<T>::erase_previous_drives(&self.drives);
        }
    }
}

impl<T: Clone + Copy + Debug> std::fmt::Debug for BucketMap<T> {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

#[derive(Debug)]
pub enum BucketMapError {
    DataNoSpace((u64, u8)),
    IndexNoSpace(u8),
}

impl<T: Clone + Copy + Debug> BucketMap<T> {
    pub fn new(config: BucketMapConfig) -> Self {
        assert_ne!(config.num_buckets, 0, "Number of buckets must be non-zero");
        assert!(
            config.num_buckets.is_power_of_two(),
            "Number of buckets must be a power of two"
        );
        let mut buckets = Vec::with_capacity(config.num_buckets);
        buckets.resize_with(config.num_buckets, || RwLock::new(None));
        let stats = Arc::new(BucketMapStats::default());
        // this should be <= 1 << DEFAULT_CAPACITY or we end up searching the same items over and over - probably not a big deal since it is so small anyway
        const MAX_SEARCH: MaxSearch = 32;
        let max_search = config.max_search.unwrap_or(MAX_SEARCH);

        if let Some(drives) = config.drives.as_ref() {
            Self::erase_previous_drives(drives);
        }
        let mut temp_dir = None;
        let drives = config.drives.unwrap_or_else(|| {
            temp_dir = Some(TempDir::new().unwrap());
            vec![temp_dir.as_ref().unwrap().path().to_path_buf()]
        });
        let drives = Arc::new(drives);

        // A simple log2 function that is correct if x is a power of two
        let log2 = |x: usize| usize::BITS - x.leading_zeros() - 1;

        Self {
            buckets,
            drives,
            num_buckets_pow2: log2(config.num_buckets) as u8,
            stats,
            max_search,
            temp_dir,
        }
    }

    fn erase_previous_drives(drives: &[PathBuf]) {
        drives.iter().for_each(|folder| {
            let _ = fs::remove_dir_all(&folder);
            let _ = fs::create_dir_all(&folder);
        })
    }

    pub fn num_buckets(&self) -> usize {
        self.buckets.len()
    }
    pub fn keys(&self, ix: usize) -> Option<Vec<Pubkey>> {
        Some(self.buckets[ix].read().unwrap().as_ref()?.keys())
    }
    pub fn bucket_len(&self, ix: usize) -> u64 {
        self.buckets[ix]
            .read()
            .unwrap()
            .as_ref()
            .map(|entry| entry.bucket_len())
            .unwrap_or_default()
    }

    pub fn range<R>(&self, ix: usize, range: Option<&R>) -> Option<Vec<BucketMapKeyValue<T>>>
    where
        R: RangeBounds<Pubkey>,
    {
        Some(self.buckets[ix].read().unwrap().as_ref()?.range(range))
    }

    pub fn values(&self, ix: usize) -> Option<Vec<Vec<T>>> {
        Some(self.buckets[ix].read().unwrap().as_ref()?.values())
    }

    pub fn read_value(&self, key: &Pubkey) -> Option<(Vec<T>, RefCount)> {
        let ix = self.bucket_ix(key);
        self.buckets[ix]
            .read()
            .unwrap()
            .as_ref()
            .and_then(|bucket| {
                bucket
                    .read_value(key)
                    .map(|(value, ref_count)| (value.to_vec(), ref_count))
            })
    }

    pub fn delete_key(&self, key: &Pubkey) {
        let ix = self.bucket_ix(key);
        if let Some(bucket) = self.buckets[ix].write().unwrap().as_mut() {
            bucket.delete_key(key);
        }
    }

    pub fn update<F>(&self, key: &Pubkey, updatefn: F)
    where
        F: Fn(Option<(&[T], RefCount)>) -> Option<(Vec<T>, RefCount)>,
    {
        let ix = self.bucket_ix(key);
        let mut bucket = self.buckets[ix].write().unwrap();
        if bucket.is_none() {
            *bucket = Some(Bucket::new(
                Arc::clone(&self.drives),
                self.max_search,
                Arc::clone(&self.stats),
            ));
        }
        let bucket = bucket.as_mut().unwrap();
        bucket.update(key, updatefn)
    }

    pub fn bucket_ix(&self, key: &Pubkey) -> usize {
        if self.num_buckets_pow2 > 0 {
            let location = read_be_u64(key.as_ref());
            (location >> (u64::BITS - self.num_buckets_pow2 as u32)) as usize
        } else {
            0
        }
    }

    pub fn addref(&self, key: &Pubkey) -> Option<RefCount> {
        let ix = self.bucket_ix(key);
        let mut bucket = self.buckets[ix].write().unwrap();
        bucket.as_mut()?.addref(key)
    }

    pub fn unref(&self, key: &Pubkey) -> Option<RefCount> {
        let ix = self.bucket_ix(key);
        let mut bucket = self.buckets[ix].write().unwrap();
        bucket.as_mut()?.unref(key)
    }
}

fn read_be_u64(input: &[u8]) -> u64 {
    assert!(input.len() >= std::mem::size_of::<u64>());
    u64::from_be_bytes(input[0..std::mem::size_of::<u64>()].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::thread_rng;
    use rand::Rng;
    use std::collections::HashMap;

    #[test]
    fn bucket_map_test_insert() {
        let key = Pubkey::new_unique();
        let config = BucketMapConfig::new(1 << 1);
        let index = BucketMap::new(config);
        index.update(&key, |_| Some((vec![0], 0)));
        assert_eq!(index.read_value(&key), Some((vec![0], 0)));
    }

    #[test]
    fn bucket_map_test_update() {
        let key = Pubkey::new_unique();
        let config = BucketMapConfig::new(1 << 1);
        let index = BucketMap::new(config);
        index.update(&key, |_| Some((vec![0], 0)));
        assert_eq!(index.read_value(&key), Some((vec![0], 0)));
        index.update(&key, |_| Some((vec![1], 0)));
        assert_eq!(index.read_value(&key), Some((vec![1], 0)));
    }

    #[test]
    fn bucket_map_test_update_to_0_len() {
        solana_logger::setup();
        let key = Pubkey::new_unique();
        let config = BucketMapConfig::new(1 << 1);
        let index = BucketMap::new(config);
        index.update(&key, |_| Some((vec![0], 1)));
        assert_eq!(index.read_value(&key), Some((vec![0], 1)));
        // sets len to 0, updates in place
        index.update(&key, |_| Some((vec![], 1)));
        assert_eq!(index.read_value(&key), Some((vec![], 1)));
        // sets len to 0, doesn't update in place - finds a new place, which causes us to no longer have an allocation in data
        index.update(&key, |_| Some((vec![], 2)));
        assert_eq!(index.read_value(&key), Some((vec![], 2)));
        // sets len to 1, doesn't update in place - finds a new place
        index.update(&key, |_| Some((vec![1], 2)));
        assert_eq!(index.read_value(&key), Some((vec![1], 2)));
    }

    #[test]
    fn bucket_map_test_delete() {
        let config = BucketMapConfig::new(1 << 1);
        let index = BucketMap::new(config);
        for i in 0..10 {
            let key = Pubkey::new_unique();
            assert_eq!(index.read_value(&key), None);

            index.update(&key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(&key), Some((vec![i], 0)));

            index.delete_key(&key);
            assert_eq!(index.read_value(&key), None);

            index.update(&key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(&key), Some((vec![i], 0)));
            index.delete_key(&key);
        }
    }

    #[test]
    fn bucket_map_test_delete_2() {
        let config = BucketMapConfig::new(1 << 2);
        let index = BucketMap::new(config);
        for i in 0..100 {
            let key = Pubkey::new_unique();
            assert_eq!(index.read_value(&key), None);

            index.update(&key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(&key), Some((vec![i], 0)));

            index.delete_key(&key);
            assert_eq!(index.read_value(&key), None);

            index.update(&key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(&key), Some((vec![i], 0)));
            index.delete_key(&key);
        }
    }

    #[test]
    fn bucket_map_test_n_drives() {
        let config = BucketMapConfig::new(1 << 2);
        let index = BucketMap::new(config);
        for i in 0..100 {
            let key = Pubkey::new_unique();
            index.update(&key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(&key), Some((vec![i], 0)));
        }
    }
    #[test]
    fn bucket_map_test_grow_read() {
        let config = BucketMapConfig::new(1 << 2);
        let index = BucketMap::new(config);
        let keys: Vec<Pubkey> = (0..100).into_iter().map(|_| Pubkey::new_unique()).collect();
        for k in 0..keys.len() {
            let key = &keys[k];
            let i = read_be_u64(key.as_ref());
            index.update(key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(key), Some((vec![i], 0)));
            for (ix, key) in keys.iter().enumerate() {
                let i = read_be_u64(key.as_ref());
                //debug!("READ: {:?} {}", key, i);
                let expected = if ix <= k { Some((vec![i], 0)) } else { None };
                assert_eq!(index.read_value(key), expected);
            }
        }
    }

    #[test]
    fn bucket_map_test_n_delete() {
        let config = BucketMapConfig::new(1 << 2);
        let index = BucketMap::new(config);
        let keys: Vec<Pubkey> = (0..20).into_iter().map(|_| Pubkey::new_unique()).collect();
        for key in keys.iter() {
            let i = read_be_u64(key.as_ref());
            index.update(key, |_| Some((vec![i], 0)));
            assert_eq!(index.read_value(key), Some((vec![i], 0)));
        }
        for key in keys.iter() {
            let i = read_be_u64(key.as_ref());
            //debug!("READ: {:?} {}", key, i);
            assert_eq!(index.read_value(key), Some((vec![i], 0)));
        }
        for k in 0..keys.len() {
            let key = &keys[k];
            index.delete_key(key);
            assert_eq!(index.read_value(key), None);
            for key in keys.iter().skip(k + 1) {
                let i = read_be_u64(key.as_ref());
                assert_eq!(index.read_value(key), Some((vec![i], 0)));
            }
        }
    }

    #[test]
    fn hashmap_compare() {
        use std::sync::Mutex;
        solana_logger::setup();
        let maps = (0..2)
            .into_iter()
            .map(|num_buckets_pow2| {
                let config = BucketMapConfig::new(1 << num_buckets_pow2);
                BucketMap::new(config)
            })
            .collect::<Vec<_>>();
        let hash_map = RwLock::new(HashMap::<Pubkey, (Vec<(usize, usize)>, RefCount)>::new());
        let max_slot_list_len = 3;
        let all_keys = Mutex::new(vec![]);

        let gen_rand_value = || {
            let count = thread_rng().gen_range(0, max_slot_list_len);
            let v = (0..count)
                .into_iter()
                .map(|x| (x as usize, x as usize /*thread_rng().gen::<usize>()*/))
                .collect::<Vec<_>>();
            let rc = thread_rng().gen::<RefCount>();
            (v, rc)
        };

        let get_key = || {
            let mut keys = all_keys.lock().unwrap();
            if keys.is_empty() {
                return None;
            }
            let len = keys.len();
            Some(keys.remove(thread_rng().gen_range(0, len)))
        };
        let return_key = |key| {
            let mut keys = all_keys.lock().unwrap();
            keys.push(key);
        };

        let verify = || {
            let mut maps = maps
                .iter()
                .map(|map| {
                    let mut r = vec![];
                    for bin in 0..map.num_buckets() {
                        r.append(
                            &mut map
                                .range(bin, None::<&std::ops::RangeInclusive<Pubkey>>)
                                .unwrap_or_default(),
                        );
                    }
                    r
                })
                .collect::<Vec<_>>();
            let hm = hash_map.read().unwrap();
            for (k, v) in hm.iter() {
                for map in maps.iter_mut() {
                    for i in 0..map.len() {
                        if k == &map[i].pubkey {
                            assert_eq!(map[i].slot_list, v.0);
                            assert_eq!(map[i].ref_count, v.1);
                            map.remove(i);
                            break;
                        }
                    }
                }
            }
            for map in maps.iter() {
                assert!(map.is_empty());
            }
        };
        let mut initial = 100; // put this many items in to start

        // do random operations: insert, update, delete, add/unref in random order
        // verify consistency between hashmap and all bucket maps
        for i in 0..10000 {
            if initial > 0 {
                initial -= 1;
            }
            if initial > 0 || thread_rng().gen_range(0, 5) == 0 {
                // insert
                let k = solana_sdk::pubkey::new_rand();
                let v = gen_rand_value();
                hash_map.write().unwrap().insert(k, v.clone());
                maps.iter().for_each(|map| {
                    map.update(&k, |current| {
                        assert!(current.is_none());
                        Some(v.clone())
                    })
                });
                return_key(k);
            }
            if thread_rng().gen_range(0, 10) == 0 {
                // update
                if let Some(k) = get_key() {
                    let hm = hash_map.read().unwrap();
                    let (v, rc) = gen_rand_value();
                    let v_old = hm.get(&k);
                    maps.iter().for_each(|map| {
                        map.update(&k, |current| {
                            assert_eq!(current, v_old.map(|(v, rc)| (&v[..], *rc)), "{}", k);
                            Some((v.clone(), rc))
                        })
                    });
                    drop(hm);
                    hash_map.write().unwrap().insert(k, (v, rc));
                    return_key(k);
                }
            }
            if thread_rng().gen_range(0, 20) == 0 {
                // delete
                if let Some(k) = get_key() {
                    let mut hm = hash_map.write().unwrap();
                    hm.remove(&k);
                    maps.iter().for_each(|map| {
                        map.delete_key(&k);
                    });
                }
            }
            if thread_rng().gen_range(0, 10) == 0 {
                // add/unref
                if let Some(k) = get_key() {
                    let mut inc = thread_rng().gen_range(0, 2) == 0;
                    let mut hm = hash_map.write().unwrap();
                    let (v, mut rc) = hm.get(&k).map(|(v, rc)| (v.to_vec(), *rc)).unwrap();
                    if !inc && rc == 0 {
                        // can't decrement rc=0
                        inc = true;
                    }
                    rc = if inc { rc + 1 } else { rc - 1 };
                    hm.insert(k, (v.to_vec(), rc));
                    maps.iter().for_each(|map| {
                        if thread_rng().gen_range(0, 2) == 0 {
                            map.update(&k, |current| Some((current.unwrap().0.to_vec(), rc)))
                        } else if inc {
                            map.addref(&k);
                        } else {
                            map.unref(&k);
                        }
                    });

                    return_key(k);
                }
            }
            if i % 1000 == 0 {
                verify();
            }
        }
        verify();
    }
}
