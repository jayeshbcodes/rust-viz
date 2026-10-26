// Rust Book ch. 15.1 — Box<T> and a recursive cons list.
#[derive(Debug)]
enum List {
    Cons(i32, Box<List>),
    Nil,
}

use List::{Cons, Nil};

fn sum(list: &List) -> i32 {
    match list {
        Cons(v, rest) => v + sum(rest),
        Nil => 0,
    }
}

fn main() {
    let b = Box::new(5);
    println!("b = {b}, *b + 1 = {}", *b + 1);
    let list = Cons(1, Box::new(Cons(2, Box::new(Cons(3, Box::new(Nil))))));
    println!("{list:?}");
    println!("sum = {}", sum(&list));
    drop(list);
    println!("list dropped early");
}
