//! Programs real `rustc` rejects (or that panic at runtime) must be caught
//! here too, with the matching error — the classic Rust Book examples.

use rust_viz::interp::{InterpError, Interpreter};

fn err_of(src: &str) -> String {
    let (_, result) = Interpreter::run_program(src);
    match result {
        Err(e) => e.to_string(),
        Ok(()) => panic!("expected an error, but the program ran:\n{src}"),
    }
}

#[test]
fn use_after_move() {
    let e = err_of("fn main() { let s1 = String::from(\"hi\"); let s2 = s1; println!(\"{s1}\"); }");
    assert!(e.contains("E0382") && e.contains("s1"), "{e}");
}

#[test]
fn use_after_move_into_function() {
    let e = err_of("fn take(s: String) {} fn main() { let s = String::from(\"hi\"); take(s); println!(\"{s}\"); }");
    assert!(e.contains("E0382"), "{e}");
}

#[test]
fn two_mutable_borrows() {
    let e = err_of("fn main() { let mut s = String::new(); let r1 = &mut s; let r2 = &mut s; println!(\"{r1}{r2}\"); }");
    assert!(e.contains("E0499"), "{e}");
}

#[test]
fn mutable_borrow_while_shared_borrow_still_used() {
    // Rust Book ch. 4.3: `s.clear()` while `word` (a slice of `s`) is still used later.
    let src = r#"
fn first_word(s: &String) -> &str { &s[0..2] }
fn main() {
    let mut s = String::from("hello world");
    let word = first_word(&s);
    s.clear();
    println!("{word}");
}"#;
    let e = err_of(src);
    assert!(e.contains("s"), "{e}");
    assert!(e.contains("borrowed"), "{e}");
}

#[test]
fn dangling_reference() {
    let src = "fn dangle() -> &String { let s = String::from(\"hi\"); &s } fn main() { let r = dangle(); }";
    assert!(err_of(src).contains("dangle"));
}

#[test]
fn push_while_iterating() {
    let src = "fn main() { let mut v = vec![1, 2]; for x in &v { v.push(*x); } }";
    assert!(err_of(src).contains("borrowed"));
}

#[test]
fn assign_to_immutable() {
    assert!(err_of("fn main() { let x = 5; x = 6; }").contains("immutable"));
}

#[test]
fn debug_needs_derive() {
    let e = err_of("struct P { x: i32 } fn main() { let p = P { x: 1 }; println!(\"{:?}\", p); }");
    assert!(e.contains("Debug"), "{e}");
}

#[test]
fn display_on_vec_is_rejected() {
    let e = err_of("fn main() { let v = vec![1]; println!(\"{}\", v); }");
    assert!(e.contains("Display"), "{e}");
}

#[test]
fn move_out_of_borrowed_field() {
    let e = err_of("struct U { name: String } fn f(u: &U) -> String { u.name } fn main() { let u = U { name: String::new() }; f(&u); }");
    assert!(e.contains("E0507"), "{e}");
}

#[test]
fn out_of_bounds_is_a_panic() {
    let (_, r) = Interpreter::run_program("fn main() { let v = vec![1, 2, 3]; let x = v[10]; }");
    assert!(matches!(r, Err(InterpError::Panic(m)) if m.contains("index out of bounds")));
}

#[test]
fn partial_move_leaves_other_fields_usable() {
    let src = r#"
struct U { name: String, email: String }
fn main() {
    let u = U { name: String::from("a"), email: String::from("b") };
    let n = u.name;
    println!("{} {}", n, u.email);
}"#;
    let (interp, r) = Interpreter::run_program(src);
    r.unwrap();
    assert_eq!(interp.output, "a b\n");
    assert_eq!(interp.steps.last().unwrap().heap.live_count(), 0);
    // ...but using the moved field again is an error.
    let e = err_of(&src.replace("u.email", "u.name"));
    assert!(e.contains("E0382"), "{e}");
}

#[test]
fn repl_borrow_ends_lazily_and_later_use_is_an_error() {
    let mut i = Interpreter::new();
    i.feed("let mut s = String::from(\"a\");").unwrap();
    i.feed("let r = &s;").unwrap();
    // Nothing uses `r` after this, so the &mut is fine (NLL)...
    i.feed("let m = &mut s;").unwrap();
    i.feed("m.push_str(\"b\");").unwrap();
    // ...but using `r` again proves the earlier borrow was still needed.
    assert!(i.feed("println!(\"{r}\");").is_err());
}
