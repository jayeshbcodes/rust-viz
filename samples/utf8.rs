use std::fmt;
struct MyString {
    bytes: Vec<u8>,
}
impl fmt::Display for MyString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let text = std::str::from_utf8(&self.bytes).unwrap();
        write!(f, "{text}")
    }
}
fn main() {
    let s = MyString { bytes: "héllo".as_bytes().to_vec() };
    println!("[{s}] [{s:>3}]");
    let v = vec![104, 105, 33];
    let t = std::str::from_utf8(&v).unwrap();
    println!("{t} {} {}", t.len(), &t[0..2]);
    let part = std::str::from_utf8(&v[1..3]);
    println!("{part:?}");
    let bad = vec![104, 255, 105];
    println!("{:?}", std::str::from_utf8(&bad));
    let st = String::from("añb");
    println!("{:?}", std::str::from_utf8(&st.as_bytes()[0..3]));
    println!("{:?}", std::str::from_utf8(&st.as_bytes()[0..2]));
    let owned = String::from_utf8(v).unwrap();
    println!("{owned} {}", owned.capacity());
    println!("{:?}", String::from_utf8(bad));
}
