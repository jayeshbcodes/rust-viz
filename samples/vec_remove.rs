fn main() {
    let mut b: Vec<(String, i32)> = vec![(String::from("a"), 1), (String::from("b"), 2), (String::from("c"), 3)];
    let pos = b.iter().position(|(k, _)| k == "a").unwrap();
    let (_, v) = b.swap_remove(pos);
    println!("{v} {:?}", b);
    let pos2 = b.iter().position(|(k, _)| k == "b").unwrap();
    let (k2, v2) = b.remove(pos2);
    println!("{k2} {v2} {:?}", b);
    for (k, v) in &mut b {
        if k == "c" {
            *v += 100;
        }
    }
    println!("{:?}", b);
}
