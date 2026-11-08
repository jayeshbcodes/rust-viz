use std::mem;
struct M {
    buckets: Vec<Vec<(String, i32)>>,
}
fn main() {
    let mut m = M { buckets: vec![vec![(String::from("a"), 1)], Vec::new()] };
    let old = std::mem::take(&mut m.buckets);
    println!("{} {}", old.len(), m.buckets.len());
    let old2 = mem::replace(&mut m.buckets, vec![Vec::new(); 4]);
    for bucket in old {
        for (k, v) in bucket {
            m.buckets[2].push((k, v));
        }
    }
    println!("{} {} {:?}", m.buckets.len(), old2.len(), m.buckets[2]);
    let mut a = String::from("x");
    let mut b = String::from("yy");
    mem::swap(&mut a, &mut b);
    let mut n = 5;
    let t = mem::take(&mut n);
    let mut name = Some(String::from("z"));
    let got = name.take();
    let got2 = mem::take(&mut name);
    println!("{a} {b} {t} {n} {got:?} {got2:?}");
}
