// Rust Book ch. 8.3 — storing keys with associated values in hash maps.
// (A HashMap's iteration order is random in real Rust, so anything printed
// from iteration is sorted first.)
use std::collections::HashMap;

fn main() {
    let mut scores = HashMap::new();
    scores.insert(String::from("Blue"), 10);
    scores.insert(String::from("Yellow"), 50);

    let team_name = String::from("Blue");
    let score = scores.get(&team_name).copied().unwrap_or(0);
    println!("Blue: {score}, Red: {}", scores.get("Red").copied().unwrap_or(0));

    let mut pairs: Vec<(&String, &i32)> = scores.iter().collect();
    pairs.sort();
    for (key, value) in &pairs {
        println!("{key}: {value}");
    }

    // Ownership: String keys/values are moved into the map.
    let field_name = String::from("Favorite color");
    let field_value = String::from("Blue");
    let mut map = HashMap::new();
    map.insert(field_name, field_value);
    println!("{:?}", map.get("Favorite color"));

    // Overwriting, and inserting only if absent.
    println!("{:?}", scores.insert(String::from("Blue"), 25));
    scores.entry(String::from("Yellow")).or_insert(50);
    scores.entry(String::from("Red")).or_insert(5);
    let mut all: Vec<_> = scores.into_iter().collect();
    all.sort();
    println!("{all:?}");

    // Updating a value based on the old one.
    let text = "hello world wonderful world";
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        let count = counts.entry(word).or_insert(0);
        *count += 1;
    }
    let mut words: Vec<_> = counts.iter().map(|(w, c)| (*w, *c)).collect();
    words.sort();
    println!("{words:?} len {}", counts.len());

    // Table growth: capacity, checked against real rustc.
    let mut m: HashMap<i32, i32> = HashMap::new();
    print!("cap {}:", m.capacity());
    for i in 0..16 {
        m.insert(i, i * i);
        print!(" {}", m.capacity());
    }
    println!();
    let from3 = HashMap::from([(1, 'a'), (2, 'b'), (3, 'c')]);
    let collected: HashMap<i32, i32> = (0..5).map(|x| (x, x * 10)).collect();
    println!("{} {} {}", from3.capacity(), collected.capacity(), HashMap::<i32, i32>::with_capacity(10).capacity());
    println!("{:?} {:?} {}", m.remove(&3), m.get(&3), m.contains_key(&4));
    if let Some(v) = m.get_mut(&4) {
        *v += 1000;
    }
    println!("{} {}", m[&4], m.len());
    let total: i32 = collected.values().sum();
    let mut keys: Vec<&i32> = collected.keys().collect();
    keys.sort();
    println!("{total} {keys:?}");
}
