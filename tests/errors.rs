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

#[test]
fn display_on_struct_without_impl_is_rejected() {
    let e = err_of("struct P { x: i32 } fn main() { let p = P { x: 1 }; println!(\"{}\", p); }");
    assert!(e.contains("Display"), "{e}");
}

#[test]
fn display_impl_runs_as_a_real_call() {
    let src = r#"
use std::fmt;
struct P { name: String }
impl fmt::Display for P {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "<{}>", self.name) }
}
fn main() { let p = P { name: String::from("a") }; println!("{p}"); println!("{}", p.name); }"#;
    let (interp, r) = Interpreter::run_program(src);
    r.unwrap();
    assert_eq!(interp.output, "<a>\na\n");
    // `fmt` got its own frame, with `self` borrowing the real `p`, not a copy.
    let in_fmt = interp.steps.iter().find(|s| s.stack.frames.iter().any(|f| f.name == "P::fmt")).unwrap();
    let self_b = in_fmt.stack.frames.iter().find(|f| f.name == "P::fmt").unwrap().get("self").unwrap();
    assert!(matches!(&self_b.value, rust_viz::value::Value::Ref(p, _) if p.name == "p"));
}

#[test]
fn fnmut_closure_needs_mut_binding() {
    let e = err_of("fn main() { let mut n = 0; let c = || n += 1; c(); }");
    assert!(e.contains("E0596") && e.contains("let mut c"), "{e}");
}

#[test]
fn using_a_variable_while_a_closure_still_holds_its_mut_borrow() {
    // Rust Book ch. 13.1: printing between defining and calling the closure.
    let e = err_of("fn main() { let mut list = vec![1]; let mut f = || list.push(7); println!(\"{list:?}\"); f(); }");
    assert!(e.contains("mutably borrowed"), "{e}");
}

#[test]
fn fnonce_closure_called_twice() {
    let e = err_of("fn main() { let s = String::from(\"a\"); let f = move || s; f(); f(); }");
    assert!(e.contains("E0382"), "{e}");
}

#[test]
fn fnonce_closure_passed_to_map() {
    let e = err_of("fn main() { let s = String::from(\"a\"); let v: Vec<String> = vec![1, 2].iter().map(move |_| s).collect(); }");
    assert!(e.contains("E0525"), "{e}");
}

#[test]
fn next_needs_mut_iterator() {
    let e = err_of("fn main() { let v = vec![1]; let it = v.iter(); it.next(); }");
    assert!(e.contains("immutable"), "{e}");
}

#[test]
fn push_while_iterator_borrow_is_live() {
    let e = err_of("fn main() { let mut v = vec![1, 2]; let it = v.iter(); v.push(3); println!(\"{:?}\", it.count()); }");
    assert!(e.contains("borrowed"), "{e}");
}

#[test]
fn closure_and_iterator_borrows_end_after_last_use() {
    let src = "fn main() { let mut v = vec![1, 2]; let total: i32 = v.iter().map(|x| x * 2).sum(); let mut add = |x| v.push(x); add(total); v.push(9); println!(\"{v:?}\"); }";
    let (interp, r) = Interpreter::run_program(src);
    r.unwrap();
    assert_eq!(interp.output, "[1, 2, 6, 9]\n");
    assert_eq!(interp.steps.last().unwrap().heap.live_count(), 0);
}

#[test]
fn as_bytes_borrows_the_string() {
    let e = err_of("fn main() { let mut s = String::from(\"hi\"); let b = s.as_bytes(); s.push('!'); println!(\"{}\", b[0]); }");
    assert!(e.contains("borrowed"), "{e}");
}

#[test]
fn repl_closure_borrow_ends_lazily() {
    let mut i = Interpreter::new();
    i.feed("let mut list = vec![1];").unwrap();
    i.feed("let mut f = || list.push(2);").unwrap();
    i.feed("f();").unwrap();
    // The closure's &mut borrow is assumed done once `list` is used...
    i.feed("println!(\"{list:?}\");").unwrap();
    assert_eq!(i.output, "[1, 2]\n");
    // ...so calling it again afterwards is the compile error it would be.
    assert!(i.feed("f();").is_err());
}

#[test]
fn hashmap_insert_moves_string_keys_and_values() {
    // Rust Book ch. 8.3: field_name is invalid after the insert.
    let e = err_of("use std::collections::HashMap; fn main() { let k = String::from(\"a\"); let mut m = HashMap::new(); m.insert(k, 1); println!(\"{k}\"); }");
    assert!(e.contains("E0382"), "{e}");
}

#[test]
fn hashmap_get_reference_blocks_insert() {
    let e = err_of("use std::collections::HashMap; fn main() { let mut m = HashMap::new(); m.insert(1, 1); let v = m.get(&1); m.insert(2, 2); println!(\"{v:?}\"); }");
    assert!(e.contains("borrowed"), "{e}");
}

#[test]
fn hashmap_index_missing_key_panics() {
    let (_, r) = Interpreter::run_program("use std::collections::HashMap; fn main() { let m: HashMap<i32, i32> = HashMap::new(); let x = m[&1]; }");
    assert!(matches!(r, Err(InterpError::Panic(m)) if m.contains("key not found")));
}

#[test]
fn hashmap_iterator_cannot_be_reversed() {
    let e = err_of("use std::collections::HashMap; fn main() { let m = HashMap::from([(1, 2)]); for x in m.iter().rev() {} }");
    assert!(e.contains("DoubleEndedIterator"), "{e}");
}

#[test]
fn hashmap_entry_borrow_ends_after_last_use_and_heap_is_freed() {
    let src = "use std::collections::HashMap; fn main() { let mut m = HashMap::new(); for w in \"a b a\".split_whitespace() { let c = m.entry(String::from(w)).or_insert(0); *c += 1; } m.insert(String::from(\"z\"), 9); println!(\"{} {}\", m[\"a\"], m.len()); }";
    let (interp, r) = Interpreter::run_program(src);
    r.unwrap();
    assert_eq!(interp.output, "2 3\n");
    assert_eq!(interp.steps.last().unwrap().heap.live_count(), 0);
}

#[test]
fn private_function_in_module() {
    let e = err_of("mod ops { fn secret() -> i32 { 1 } } fn main() { let x = ops::secret(); }");
    assert!(e.contains("E0603") && e.contains("secret"), "{e}");
}

#[test]
fn private_module() {
    let e = err_of("mod a { mod b { pub fn f() {} } } fn main() { a::b::f(); }");
    assert!(e.contains("E0603") && e.contains("module `b`"), "{e}");
}

#[test]
fn file_module_is_reported() {
    let e = err_of("mod ops; fn main() {}");
    assert!(e.contains("mod ops { ... }"), "{e}");
}

#[test]
fn private_items_visible_to_children() {
    // A child module may use its parent's private items (and its own
    // module's, unqualified); a glob `use` brings a module's items in.
    let src = r#"
mod outer {
    fn helper() -> i32 { 40 }
    pub mod inner {
        pub fn get() -> i32 { super::helper() + two() }
        fn two() -> i32 { 2 }
    }
}
use outer::inner::*;
fn main() { println!("{}", get()); }
"#;
    let (interp, result) = Interpreter::run_program(src);
    result.unwrap();
    assert_eq!(interp.output, "42\n");
}
