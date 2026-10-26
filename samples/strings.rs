// Note: a bare statement block, not a standalone file — `Interpreter::run`
// wraps it in `{ ... }` itself, and the golden test wraps it in `fn main()`
// before handing it to `rustc`. Revisit once the interpreter parses whole
// `fn main() { ... }` programs directly.
let mut s = String::from("hi");
s.push_str(" there");
println!("{s}");
println!("{}", s.len());
