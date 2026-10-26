// Rust Book ch. 4.3 — string slices.
fn first_word(s: &str) -> &str {
    let bytes_len = s.len();
    let mut i = 0;
    for c in s.chars() {
        if c == ' ' {
            return &s[0..i];
        }
        i += 1;
    }
    &s[..bytes_len]
}

fn main() {
    let my_string = String::from("hello world");
    let word = first_word(&my_string[..]);
    println!("first word: {word}");

    let literal = "hello rustaceans";
    let w2 = first_word(literal);
    println!("{w2}");

    let a = [1, 2, 3, 4, 5];
    let slice = &a[1..3];
    println!("{:?} {}", slice, slice.len());
}
