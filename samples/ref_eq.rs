fn main() {
    let a = String::from("apple");
    let b = String::from("apple");
    let ra = &a;
    let rb = &b;
    let ra2 = &a;
    println!("{} {} {} {}", ra == rb, ra == "apple", std::ptr::eq(ra, rb), std::ptr::eq(ra, ra2));
    let v = vec![1, 1];
    println!("{} {}", &v[0] == &v[1], std::ptr::eq(&v[0], &v[1]));
}
