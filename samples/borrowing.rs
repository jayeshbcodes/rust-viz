// Rust Book ch. 4.2 — references, &mut, and non-lexical lifetimes.
fn main() {
    let s1 = String::from("hello");
    let len = calculate_length(&s1);
    println!("The length of '{s1}' is {len}.");

    let mut s = String::from("hello");
    change(&mut s);

    let r1 = &s;
    let r2 = &s;
    println!("{r1} and {r2}");
    // r1 and r2 are never used again, so a &mut is fine here.
    let r3 = &mut s;
    r3.push_str("!");
    println!("{r3}");
    println!("{}", s.len());
}

fn calculate_length(s: &String) -> usize {
    s.len()
}

fn change(some_string: &mut String) {
    some_string.push_str(", world");
}
