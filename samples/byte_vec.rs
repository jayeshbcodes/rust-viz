struct MyString {
    bytes: Vec<u8>,
}
impl MyString {
    fn new() -> Self {
        MyString { bytes: Vec::new() }
    }
    fn from(s: &str) -> Self {
        MyString { bytes: s.as_bytes().to_vec() }
    }
    fn push_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }
    fn push_byte(&mut self, b: u8) {
        self.bytes.push(b);
    }
    fn report(&self) {
        println!("len {} cap {}", self.bytes.len(), self.bytes.capacity());
    }
}
fn main() {
    let mut a = MyString::from("a");
    a.report();
    a.push_str("b");
    a.report();
    a.push_str("cdefghi");
    a.report();
    let mut e = MyString::new();
    e.push_byte(104);
    e.report();
    e.push_str("ello world");
    e.report();
    let mut v: Vec<u8> = Vec::new();
    v.push(1);
    println!("v cap {}", v.capacity());
    let mut w = vec![1, 2];
    w.extend_from_slice(&[3, 4, 5]);
    println!("{:?} cap {}", w, w.capacity());
    let names = vec![String::from("x")];
    let mut more = vec![String::from("y")];
    more.extend_from_slice(&names);
    println!("{:?} {:?} cap {}", names, more, more.capacity());
    let c = a.bytes.clone();
    let mut c2 = c;
    c2.push(1);
    println!("clone cap {}", c2.capacity());
}
