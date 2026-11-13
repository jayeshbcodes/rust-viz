use std::borrow::Borrow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

struct Map<K, V> {
    buckets: Vec<Vec<(K, V)>>,
}

fn bucket_of<Q: Hash + ?Sized>(key: &Q, n: usize) -> usize {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    (h.finish() % n as u64) as usize
}

impl<K: Hash + Eq, V> Map<K, V> {
    fn new(n: usize) -> Self {
        Map { buckets: (0..n).map(|_| Vec::new()).collect() }
    }
    fn insert(&mut self, key: K, value: V) {
        let i = bucket_of(&key, self.buckets.len());
        self.buckets[i].push((key, value));
    }
    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = bucket_of(key, self.buckets.len());
        for (k, v) in &self.buckets[i] {
            let kq: &Q = k.borrow();
            if kq == key {
                return Some(v);
            }
        }
        None
    }
}

fn main() {
    let mut m = Map::new(8);
    m.insert(String::from("apple"), 3);
    m.insert(String::from("pear"), 5);
    let owned = String::from("pear");
    println!("{:?} {:?} {:?}", m.get("apple"), m.get(&owned), m.get("plum"));
    let mut n: Map<i32, &str> = Map::new(4);
    n.insert(7, "seven");
    println!("{:?} {}", n.get(&7), bucket_of("apple", 8));
    let v = vec![1, 2, 3];
    let s: &[i32] = v.borrow();
    println!("{:?}", s);
}
