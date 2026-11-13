use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
#[derive(Hash)]
struct Point {
    x: i32,
    y: i32,
}
#[derive(Hash)]
enum Shape {
    Dot,
    Circle(i32),
    Rect { w: i32, h: i32 },
}
fn h<T: Hash + ?Sized>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}
fn main() {
    let owned = String::from("apple");
    println!("{} {} {}", h("apple"), h(&owned), h(&owned) == h("apple"));
    println!("{} {} {}", h(&42), h(&-7), h(&'é'));
    println!("{} {}", h(&true), h(&()));
    println!("{} {}", h(&(1, "a")), h(&[1, 2, 3]));
    let v = vec![1, 2, 3];
    println!("{} {}", h(&v), h(&v[..2]));
    println!("{} {}", h(&Some(5)), h(&None::<i32>));
    let ok: Result<i32, String> = Ok(1);
    println!("{}", h(&ok));
    println!("{} {}", h(&Point { x: 1, y: 2 }), h(&Box::new(9)));
    println!("{} {} {}", h(&Shape::Dot), h(&Shape::Circle(3)), h(&Shape::Rect { w: 1, h: 2 }));
    println!("{}", h(&vec![String::from("a"), String::from("bc")]));
    println!("{}", h(&"apple".as_bytes()[1..3]));
    let n = 8;
    println!("{}", (h("apple") % n as u64) as usize);
}
