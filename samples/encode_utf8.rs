fn main() {
    let mut v: Vec<u8> = Vec::new();
    for c in ['h', 'é', '€', '😀'] {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        println!("{} {}", s, s.len());
        v.extend_from_slice(s.as_bytes());
        println!("{:?}", buf);
    }
    println!("{:?} {}", v, v.capacity());
    let mut small = [0u8; 4];
    let t: &str = 'x'.encode_utf8(&mut small);
    let owned = t.to_string();
    println!("{owned} {}", std::str::from_utf8(&small[..1]).unwrap());
}
