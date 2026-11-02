fn last_char(bytes: &[u8], start: usize) -> Option<char> {
    let s = std::str::from_utf8(&bytes[start..]).ok()?;
    s.chars().next()
}
fn parse_len(r: Result<String, String>) -> Result<usize, usize> {
    r.map(|s| s.len()).map_err(|e| e.len() * 100)
}
fn main() {
    let v = vec![104, 195, 169];
    println!("{:?} {:?}", last_char(&v, 1), last_char(&v, 2));
    let good: Result<String, String> = Ok(String::from("abc"));
    let bad: Result<String, String> = Err(String::from("no"));
    println!("{:?} {:?}", parse_len(good), parse_len(bad));
    let n: Result<i32, String> = Ok(4);
    println!("{:?}", n.and_then(|x| if x > 3 { Ok(x * 10) } else { Err(String::from("small")) }));
    let e: Result<i32, String> = Err(String::from("boom"));
    println!("{} {:?}", e.clone().unwrap_or_else(|m| m.len() as i32), e.err());
    let k: Result<i32, String> = Ok(7);
    println!("{} {:?}", k.clone().is_ok_and(|x| x > 5), k.ok());
    let ue: Result<i32, String> = Err(String::from("x"));
    println!("{}", ue.is_ok_and(|x| x > 5));
}
