// collect()/join capacities and closure Copy-ness, checked against rustc.
fn main() {
    let v = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let a: Vec<i32> = v.iter().map(|x| x * 2).collect();
    println!("map collect cap {}", a.capacity());
    let b: Vec<&i32> = v.iter().filter(|x| **x % 2 == 0).collect();
    println!("filter(5) cap {}", b.capacity());
    let c: Vec<i32> = v.iter().filter(|x| **x > 0).cloned().collect();
    println!("filter(10) cap {}", c.capacity());
    let d: Vec<i32> = (0..7).collect();
    println!("range cap {}", d.capacity());
    let e: Vec<i32> = v.iter().skip(2).cloned().collect();
    println!("skip cap {}", e.capacity());
    let s: String = "hello".chars().rev().collect();
    println!("chars collect {} cap {}", s, s.capacity());
    let s2: String = vec!["ab", "cd", "ef"].into_iter().collect();
    println!("strs collect cap {}", s2.capacity());
    let words = vec![String::from("hi"), String::from("there")];
    let joined = words.join(" ");
    println!("join cap {}", joined.capacity());
    let e2: Vec<i32> = v.iter().take_while(|x| **x < 4).cloned().collect();
    println!("take_while cap {}", e2.capacity());
    let n = 5;
    let add = |x: i32| x + n;
    let add2 = add; // Fn closure capturing &i32 is Copy
    println!("{} {}", add(1), add2(1));
    let w = vec![10, 20, 30];
    let inplace: Vec<i32> = w.into_iter().map(|x| x + 1).collect();
    println!("in-place cap {}", inplace.capacity());
    let empty: Vec<i32> = v.iter().filter(|x| **x > 100).cloned().collect();
    println!("empty cap {}", empty.capacity());
}
