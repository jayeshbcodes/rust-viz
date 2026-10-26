// Rust Book ch. 3.5 — if/else, loop, while, for.
fn main() {
    let number = 7;
    if number % 2 == 0 {
        println!("even");
    } else if number > 5 {
        println!("odd and big");
    } else {
        println!("odd and small");
    }
    let parity = if number % 2 == 0 { "even" } else { "odd" };
    println!("{parity}");

    let mut counter = 0;
    let result = loop {
        counter += 1;
        if counter == 4 {
            break counter * 2;
        }
    };
    println!("loop result {result}");

    let mut n = 3;
    while n != 0 {
        println!("{n}!");
        n -= 1;
    }

    let mut names = vec![String::from("a"), String::from("b")];
    for name in &mut names {
        name.push_str("!");
    }
    for (i, name) in names.iter().enumerate() {
        let label = format!("{i}: {name}");
        println!("{label}");
    }
    for name in names {
        println!("consumed {name}");
    }

    let mut total = 0;
    'outer: for i in 1..=3 {
        for j in (0..3).rev() {
            if i * j == 4 {
                break 'outer;
            }
            total += i * j;
        }
    }
    println!("total {total}");
}
