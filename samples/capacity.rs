// Heap growth: Vec/String capacity doubling, checked against real rustc.
fn main() {
    let mut v: Vec<i32> = Vec::new();
    println!("new: len {} cap {}", v.len(), v.capacity());
    for i in 0..9 {
        v.push(i);
        println!("push {i}: len {} cap {}", v.len(), v.capacity());
    }
    let mut s = String::from("hello");
    println!("{} {}", s.len(), s.capacity());
    s.push_str(", world");
    println!("{} {}", s.len(), s.capacity());
    let c = s.clone();
    println!("clone cap {}", c.capacity());
    let s2 = String::from("ab") + "cdef";
    println!("{s2} {}", s2.capacity());
    let mut e = String::new();
    e.push('x');
    println!("{}", e.capacity());
    let w = Vec::<i32>::with_capacity(10);
    println!("{}", w.capacity());
    v.clear();
    println!("cleared: len {} cap {}", v.len(), v.capacity());
}
