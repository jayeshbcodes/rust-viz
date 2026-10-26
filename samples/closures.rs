// Rust Book ch. 13.1 — closures and how they capture.
#[derive(Debug)]
struct Rectangle {
    width: u32,
    height: u32,
}

fn apply<F: Fn(i32) -> i32>(f: F, x: i32) -> i32 {
    f(x)
}

fn make_adder(n: i32) -> impl Fn(i32) -> i32 {
    move |x| x + n
}

fn double(x: i32) -> i32 {
    x * 2
}

fn main() {
    // Captures `list` by shared reference.
    let list = vec![1, 2, 3];
    let only_borrows = || println!("From closure: {list:?}");
    println!("Before calling closure: {list:?}");
    only_borrows();
    println!("After calling closure: {list:?}");

    // Captures `list` by mutable reference: FnMut, needs `let mut`.
    let mut list = vec![1, 2, 3];
    let mut borrows_mutably = || list.push(7);
    borrows_mutably();
    println!("After calling closure: {list:?}");

    // `move` with state that lives inside the closure.
    let mut count = 0;
    let mut counter = move || {
        count += 1;
        count
    };
    println!("{} {} {}", counter(), counter(), counter());
    println!("outer count is still {count}");

    // FnOnce: moves a captured String out.
    let name = String::from("ferris");
    let consume = move || name;
    let got = consume();
    println!("{got}");

    // sort_by_key with an FnMut closure counting its calls.
    let mut list = [
        Rectangle { width: 10, height: 1 },
        Rectangle { width: 3, height: 5 },
        Rectangle { width: 7, height: 12 },
    ];
    let mut num_sort_operations = 0;
    let mut v: Vec<Rectangle> = Vec::new();
    for r in list {
        v.push(r);
    }
    v.sort_by_key(|r| {
        num_sort_operations += 1;
        r.width
    });
    println!("{v:#?}, sorted in {num_sort_operations} operations");

    let add5 = make_adder(5);
    println!("{} {} {}", add5(1), apply(&add5, 10), apply(double, 21));
    let boxed: Box<dyn Fn(i32) -> i32> = Box::new(|x| x - 1);
    println!("{}", boxed(100));
    let maybe = Some(4).map(|x| x * 10).unwrap_or_else(|| 0);
    println!("{maybe}");
}
