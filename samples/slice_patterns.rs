#[derive(Debug)]
enum Token {
    Num(f64),
    Op(char),
}
fn parse(tokens: &[Token]) -> String {
    match tokens {
        [Token::Num(l), Token::Op(o), Token::Num(r)] => format!("{l} {o} {r}"),
        [] => String::from("empty"),
        [first, ..] => format!("starts with {first:?}"),
    }
}
fn describe(v: &[i32]) -> String {
    match v {
        [] => String::from("none"),
        [x] => format!("one: {x}"),
        [first, .., last] => format!("{first}..{last}"),
    }
}
fn main() {
    let t = vec![Token::Num(3.0), Token::Op('+'), Token::Num(4.0)];
    println!("{}", parse(&t));
    println!("{}", parse(&t[1..]));
    println!("{}", parse(&[]));
    let nums = vec![1, 2, 3, 4];
    println!("{} {} {}", describe(&nums), describe(&nums[..1]), describe(&[]));
    if let [head, rest @ ..] = nums.as_slice() {
        println!("{head} {:?} {}", rest, rest.len());
    }
    let arr = [10, 20, 30];
    let [a, b, c] = arr;
    println!("{}", a + b + c);
    match &nums[..] {
        [_, second, ..] if *second > 1 => println!("second {second}"),
        _ => println!("other"),
    }
}
