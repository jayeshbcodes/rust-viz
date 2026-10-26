// Rust Book ch. 13.2 — iterators, adaptors, consumers.
#[derive(PartialEq, Debug, Clone)]
struct Shoe {
    size: u32,
    style: String,
}

fn shoes_in_size(shoes: Vec<Shoe>, shoe_size: u32) -> Vec<Shoe> {
    shoes.into_iter().filter(|s| s.size == shoe_size).collect()
}

fn first_word(s: &String) -> usize {
    let bytes = s.as_bytes();
    for (i, &item) in bytes.iter().enumerate() {
        if item == b' ' {
            return i;
        }
    }
    s.len()
}

fn main() {
    let v1 = vec![1, 2, 3];
    let mut v1_iter = v1.iter();
    println!("{:?} {:?}", v1_iter.next(), v1_iter.next());
    let total: i32 = v1.iter().sum();
    println!("total {total}");

    let v2: Vec<i32> = v1.iter().map(|x| x + 1).collect();
    println!("{v2:?} cap {}", v2.capacity());

    let shoes = vec![
        Shoe { size: 10, style: String::from("sneaker") },
        Shoe { size: 13, style: String::from("sandal") },
        Shoe { size: 10, style: String::from("boot") },
    ];
    let in_my_size = shoes_in_size(shoes, 10);
    println!("{in_my_size:?}");

    let words = "the quick brown fox";
    let lens: Vec<usize> = words.split_whitespace().map(|w| w.len()).collect();
    println!("{lens:?}");
    let caps: String = words.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_uppercase()).collect();
    println!("{caps}");

    let evens: Vec<i32> = (1..=10).filter(|n| n % 2 == 0).collect();
    println!("{evens:?} cap {}", evens.capacity());
    let sum_sq: i32 = (1..=4).map(|x| x * x).sum();
    let prod: i32 = (1..=5).product();
    println!("{sum_sq} {prod}");
    let pairs: Vec<(i32, char)> = vec![1, 2, 3].into_iter().zip("abc".chars()).collect();
    println!("{pairs:?}");
    let max = v1.iter().max();
    let pos = v1.iter().position(|&x| x == 2);
    let found = v1.iter().find(|&&x| x > 1);
    println!("{max:?} {pos:?} {found:?} {}", v1.iter().any(|&x| x > 2));
    let folded = v1.iter().fold(String::new(), |acc, x| acc + &x.to_string());
    println!("{folded}");
    for (i, x) in v1.iter().enumerate().skip(1) {
        println!("{i}: {x}");
    }
    let rev: Vec<i32> = v1.iter().rev().cloned().collect();
    println!("{rev:?} {}", v1.iter().count());
    let s = String::from("hello world");
    println!("first word ends at {}", first_word(&s));
    let joined = vec!["a", "b", "c"].join("-");
    println!("{joined}");
}
