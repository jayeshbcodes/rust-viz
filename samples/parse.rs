fn main() {
    println!("{:?} {:?} {:?}", "42".parse::<i32>(), "-7".parse::<i64>(), "3.5".parse::<f64>());
    let n: i32 = "123".parse().unwrap();
    let r: Result<u8, _> = "300".parse();
    println!("{} {:?}", n + 1, r);
    println!("{:?}", "x".parse::<bool>());
    println!("{:?}", "ab".parse::<char>());
    println!("{:?}", "".parse::<f64>());
    println!("{:?}", "1.5x".parse::<f64>());
    println!("{:?}", "-1".parse::<u32>());
    println!("{:?} {:?}", "true".parse::<bool>(), "é".parse::<char>());
    match "x".parse::<i32>() {
        Err(e) => println!("{e}"),
        Ok(_) => {}
    }
    match "".parse::<f64>() {
        Err(e) => println!("{}", e.to_string()),
        Ok(_) => {}
    }
    let s = String::from(" 8 ");
    let total: i32 = s.trim().parse::<i32>().unwrap() * 2;
    println!("{total}");
    let words = "3 4 x 5";
    let mut sum = 0;
    for w in words.split_whitespace() {
        if let Ok(v) = w.parse::<i32>() {
            sum += v;
        }
    }
    println!("{sum}");
    let owned: String = "hi".parse().unwrap();
    println!("{owned}");
}
